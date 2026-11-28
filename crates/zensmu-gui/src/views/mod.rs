//! Dark cockpit UI panels, plots, tuning controls, and confirmation modals.

mod bench;
mod chrome;
mod curve;
mod dashboard;
mod modals;
mod power;
mod profiles;
mod shaper;
mod theme;
mod widgets;

pub use chrome::{render_banners, render_bottom_bar, render_sidebar, render_top_bar};
pub use curve::render_curve_optimizer;
pub use dashboard::render_dashboard;
pub use modals::{render_dry_run_modal, render_persist_confirm_modal, render_report_modal};
pub use power::render_pbo_limits;
pub use profiles::render_profiles_and_safety;
pub use shaper::render_curve_shaper;
pub use theme::*;
#[cfg(test)]
pub use widgets::compact_cpu_label;
