//! Application shell chrome: top bar, bottom bar, left sidebar, and status banners.

mod banners;
mod footer;
mod header;
mod sidebar;

pub use banners::render_banners;
pub use footer::render_bottom_bar;
pub use header::render_top_bar;
pub use sidebar::render_sidebar;
