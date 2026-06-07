//! Provides interface for controlling the terminal

use std::io;

use crate::{buffer::Cell, terminal::Config};

use helix_view::{
    graphics::{CursorKind, Rect},
    theme::Color,
};

#[cfg(all(feature = "termina", not(windows)))]
mod termina;
#[cfg(all(feature = "termina", not(windows)))]
pub use self::termina::TerminaBackend;

#[cfg(all(feature = "termina", windows))]
mod crossterm;
#[cfg(all(feature = "termina", windows))]
pub use self::crossterm::CrosstermBackend;

mod test;
pub use self::test::TestBackend;

/// Representation of a terminal backend.
pub trait Backend {
    /// Claims the terminal for TUI use.
    fn claim(&mut self) -> Result<(), io::Error>;
    /// Update terminal configuration.
    fn reconfigure(&mut self, config: Config) -> Result<(), io::Error>;
    /// Restores the terminal to a normal state, undoes `claim`
    fn restore(&mut self) -> Result<(), io::Error>;
    /// Draws styled text to the terminal
    fn draw<'a, I>(&mut self, content: I) -> Result<(), io::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>;
    /// Hides the cursor
    fn hide_cursor(&mut self) -> Result<(), io::Error>;
    /// Sets the cursor to the given shape
    fn show_cursor(&mut self, kind: CursorKind) -> Result<(), io::Error>;
    /// Sets the cursor to the given position
    fn set_cursor(&mut self, x: u16, y: u16) -> Result<(), io::Error>;
    /// Clears the terminal
    fn clear(&mut self) -> Result<(), io::Error>;
    /// Gets the size of the terminal in cells
    fn size(&self) -> Result<Rect, io::Error>;
    /// Flushes the terminal buffer
    fn flush(&mut self) -> Result<(), io::Error>;
    fn supports_true_color(&self) -> bool;
    fn get_theme_mode(&self) -> Option<helix_view::theme::Mode>;
    fn set_background_color(&mut self, color: Option<Color>) -> io::Result<()>;

    /// Pixel size `(width, height)` of a single terminal cell, if the platform
    /// reports pixel dimensions. Used to size inline graphics (e.g. Jupyter plots).
    fn cell_pixel_size(&self) -> Option<(u16, u16)> {
        None
    }
    /// Whether the terminal supports the kitty graphics protocol with Unicode
    /// placeholders, used to render inline images.
    fn supports_graphics(&self) -> bool {
        false
    }
    /// Transmit a base64-encoded PNG to the terminal as kitty image `id`, creating
    /// a virtual placement spanning `cols`×`rows` cells (for Unicode placeholders).
    fn transmit_image(
        &mut self,
        _id: u32,
        _cols: u16,
        _rows: u16,
        _base64_png: &str,
    ) -> Result<(), io::Error> {
        Ok(())
    }
    /// Delete a previously transmitted kitty image and all of its placements.
    fn delete_image(&mut self, _id: u32) -> Result<(), io::Error> {
        Ok(())
    }
}
