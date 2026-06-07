use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use jupyter_protocol::{
    ConnectionInfo, ExecuteRequest, InputReply, InterruptRequest, JupyterMessage,
    JupyterMessageContent, KernelInfoRequest, ShutdownRequest, Transport,
};
use jupyter_zmq_client::{
    create_client_control_connection, create_client_iopub_connection,
    create_client_shell_connection_with_identity, create_client_stdin_connection_with_identity,
    find_kernelspec_with_jupyter_paths, peek_ports_with_listeners, peer_identity_for_session,
    Connection,
};
use tokio::process::Child;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use uuid::Uuid;

use crate::messages::{Channel, Payload};
use crate::registry::KernelId;
use crate::{Error, Result};

/// Timeout for the initial `kernel_info_request` handshake.
const KERNEL_INFO_TIMEOUT: Duration = Duration::from_secs(20);

/// A connected, running Jupyter kernel.
///
/// Mirrors `helix_dap::Client`: outgoing requests are pushed onto an unbounded
/// channel drained by a background send task, while a set of receive tasks
/// forward incoming messages as `(KernelId, Payload)` to the registry's merged
/// stream. State persists in the kernel between executions.
#[derive(Debug)]
pub struct Client {
    id: KernelId,
    name: String,
    session_id: String,
    connection_info: ConnectionInfo,
    outgoing: UnboundedSender<(Channel, JupyterMessage)>,
    _process: Child,
    connection_file: PathBuf,
}

impl Client {
    pub fn id(&self) -> KernelId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Discover the kernelspec named `kernel_name`, spawn it, connect all
    /// channels, and perform the `kernel_info` handshake.
    ///
    /// Returns the client plus the receiver of incoming messages, which the
    /// registry pushes onto its merged `incoming` stream.
    pub async fn start(
        id: KernelId,
        kernel_name: &str,
    ) -> Result<(Self, UnboundedReceiver<(KernelId, Payload)>)> {
        let spec = find_kernelspec_with_jupyter_paths(kernel_name).await?;

        // Reserve five ports, keeping the listeners alive until the kernel
        // process has been spawned (closes the TOCTOU window — see
        // `peek_ports_with_listeners`).
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let (ports, listeners) = peek_ports_with_listeners(ip, 5).await?;

        let session_id = Uuid::new_v4().to_string();
        let connection_info = ConnectionInfo {
            ip: ip.to_string(),
            transport: Transport::TCP,
            shell_port: ports[0],
            iopub_port: ports[1],
            stdin_port: ports[2],
            control_port: ports[3],
            hb_port: ports[4],
            key: Uuid::new_v4().to_string(),
            signature_scheme: "hmac-sha256".to_string(),
            kernel_name: Some(kernel_name.to_string()),
        };

        // Write the connection file the kernel reads on launch.
        let connection_file =
            std::env::temp_dir().join(format!("helix-kernel-{}.json", Uuid::new_v4()));
        let json = serde_json::to_vec(&connection_info)?;
        tokio::fs::write(&connection_file, json).await?;

        // Spawn the kernel. `command` substitutes `{connection_file}` in argv.
        let mut command =
            spec.command(&connection_file, Some(Stdio::null()), Some(Stdio::null()))?;
        command.kill_on_drop(true);
        let process = command.spawn().map_err(|err| {
            Error::Other(anyhow::anyhow!(
                "failed to spawn kernel {kernel_name}: {err}"
            ))
        })?;
        // The kernel binds the ports as its first action; release ours now.
        drop(listeners);

        let key = &connection_info.key;
        let peer_identity = peer_identity_for_session(&session_id)?;

        let mut shell = create_client_shell_connection_with_identity(
            &connection_info,
            &session_id,
            peer_identity.clone(),
        )
        .await?;
        let control = create_client_control_connection(&connection_info, &session_id).await?;
        let stdin = create_client_stdin_connection_with_identity(
            &connection_info,
            &session_id,
            peer_identity,
        )
        .await?;
        let iopub = create_client_iopub_connection(&connection_info, "", &session_id).await?;
        let _ = key;

        // Handshake: send kernel_info on shell and wait for the reply. This also
        // acts as a barrier so iopub is subscribed before the first execution.
        shell
            .send(JupyterMessage::new(KernelInfoRequest {}, None))
            .await?;
        wait_for_kernel_info(&mut shell).await?;

        // Split the duplex channels into send/recv halves so a single send task
        // and per-channel recv tasks can run concurrently.
        let (shell_tx, shell_rx) = shell.split();
        let (control_tx, control_rx) = control.split();
        let (stdin_tx, stdin_rx) = stdin.split();

        let (outgoing, outgoing_rx) = unbounded_channel();
        let (client_tx, client_rx) = unbounded_channel();

        tokio::spawn(send_task(shell_tx, control_tx, stdin_tx, outgoing_rx));
        spawn_recv(id, shell_rx, Payload::Shell, client_tx.clone());
        spawn_recv(id, control_rx, Payload::Control, client_tx.clone());
        spawn_recv(id, stdin_rx, Payload::Stdin, client_tx.clone());
        spawn_recv(id, iopub, Payload::IoPub, client_tx);

        let client = Self {
            id,
            name: kernel_name.to_string(),
            session_id,
            connection_info,
            outgoing,
            _process: process,
            connection_file,
        };

        Ok((client, client_rx))
    }

    fn send(&self, channel: Channel, message: JupyterMessage) -> Result<()> {
        self.outgoing
            .send((channel, message))
            .map_err(|_| Error::StreamClosed)
    }

    /// Execute `code` in the kernel. Returns the request `msg_id`, which is the
    /// `parent_header.msg_id` carried by every resulting iopub message and the
    /// final `execute_reply`, used to correlate output back to this execution.
    pub fn execute(&self, code: String, silent: bool) -> Result<String> {
        let mut request = ExecuteRequest::new(code);
        request.silent = silent;
        request.store_history = !silent;
        let message: JupyterMessage = request.into();
        let msg_id = message.header.msg_id.clone();
        self.send(Channel::Shell, message)?;
        Ok(msg_id)
    }

    /// Execute `code` without storing it in history or incrementing the
    /// execution count, used for the variable-introspection follow-up. Stream
    /// output is still emitted (unlike `silent`). Returns the request `msg_id`.
    pub fn execute_quiet(&self, code: String) -> Result<String> {
        let mut request = ExecuteRequest::new(code);
        request.silent = false;
        request.store_history = false;
        let message: JupyterMessage = request.into();
        let msg_id = message.header.msg_id.clone();
        self.send(Channel::Shell, message)?;
        Ok(msg_id)
    }

    /// Reply to an `input_request` (from a `StdinRequest` payload).
    pub fn input_reply(&self, value: String) -> Result<()> {
        let reply = InputReply {
            value,
            ..Default::default()
        };
        let message: JupyterMessage = reply.into();
        self.send(Channel::Stdin, message)
    }

    /// Interrupt the running execution via the control channel.
    pub fn interrupt(&self) -> Result<()> {
        let message: JupyterMessage = InterruptRequest {}.into();
        self.send(Channel::Control, message)
    }

    /// Shut down (or restart) the kernel via the control channel.
    pub fn shutdown(&self, restart: bool) -> Result<()> {
        let message: JupyterMessage = ShutdownRequest { restart }.into();
        self.send(Channel::Control, message)
    }

    pub fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Best-effort cleanup of the connection file; the process is killed via
        // `kill_on_drop`.
        let _ = std::fs::remove_file(&self.connection_file);
    }
}

async fn wait_for_kernel_info(shell: &mut jupyter_zmq_client::ClientShellConnection) -> Result<()> {
    let deadline = tokio::time::Instant::now() + KERNEL_INFO_TIMEOUT;
    loop {
        let read = tokio::time::timeout_at(deadline, shell.read())
            .await
            .map_err(|_| Error::Timeout)??;
        if matches!(read.content, JupyterMessageContent::KernelInfoReply(_)) {
            return Ok(());
        }
    }
}

async fn send_task(
    mut shell: jupyter_zmq_client::DealerSendConnection,
    mut control: jupyter_zmq_client::DealerSendConnection,
    mut stdin: jupyter_zmq_client::DealerSendConnection,
    mut rx: UnboundedReceiver<(Channel, JupyterMessage)>,
) {
    while let Some((channel, message)) = rx.recv().await {
        let result = match channel {
            Channel::Shell => shell.send(message).await,
            Channel::Control => control.send(message).await,
            Channel::Stdin => stdin.send(message).await,
        };
        if let Err(err) = result {
            log::error!("failed to send jupyter message: {err}");
        }
    }
}

/// Spawn a task that forwards every message read from `conn` as a `Payload`.
fn spawn_recv<S>(
    id: KernelId,
    mut conn: Connection<S>,
    wrap: fn(JupyterMessage) -> Payload,
    tx: UnboundedSender<(KernelId, Payload)>,
) where
    S: zeromq::SocketRecv + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match conn.read().await {
                Ok(message) => {
                    if tx.send((id, wrap(message))).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    log::debug!("jupyter recv channel closed: {err}");
                    break;
                }
            }
        }
    });
}
