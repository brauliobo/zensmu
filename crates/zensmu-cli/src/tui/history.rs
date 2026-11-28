//! Rolling history ring buffer and telemetry statistics.

use std::collections::VecDeque;
use zensmu_core::ops::pm_table::TelemetrySnapshot;

// ---------------------------------------------------------------------------
// Rolling history ring buffer & statistics
// ---------------------------------------------------------------------------

/// Fixed-capacity rolling history buffer for live telemetry series.
#[derive(Debug, Clone)]
pub(crate) struct HistoryBuffer {
    pub(super) capacity: usize,
    pub(super) samples: VecDeque<f64>,
}

impl HistoryBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            capacity: cap,
            samples: VecDeque::with_capacity(cap),
        }
    }

    pub(crate) fn push(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(value);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.samples.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn latest(&self) -> Option<f64> {
        self.samples.back().copied()
    }

    pub(crate) fn min(&self) -> Option<f64> {
        self.samples.iter().copied().reduce(f64::min)
    }

    pub(crate) fn max(&self) -> Option<f64> {
        self.samples.iter().copied().reduce(f64::max)
    }

    pub(crate) fn avg(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        let sum: f64 = self.samples.iter().sum();
        Some(sum / self.samples.len() as f64)
    }

    /// Returns `(min, avg, max)` across all buffered samples.
    pub(crate) fn stats(&self) -> Option<(f64, f64, f64)> {
        Some((self.min()?, self.avg()?, self.max()?))
    }

    /// Convert the trailing `max_points` samples into non-negative `u64` values
    /// scaled by `scale` for `ratatui::widgets::Sparkline`.
    pub(crate) fn sparkline_window(&self, max_points: usize, scale: f64) -> Vec<u64> {
        if max_points == 0 || self.samples.is_empty() {
            return Vec::new();
        }
        let skip = self.samples.len().saturating_sub(max_points);
        self.samples
            .iter()
            .skip(skip)
            .map(|&v| {
                let scaled = (v * scale).max(0.0).round();
                if scaled.is_finite() {
                    scaled as u64
                } else {
                    0
                }
            })
            .collect()
    }
}

/// Rolling histories for all tracked PM table telemetry metrics.
#[derive(Debug, Clone)]
pub(super) struct TelemetryHistory {
    pub(super) ppt_w: HistoryBuffer,
    pub(super) tctl_c: HistoryBuffer,
    pub(super) tdc_a: HistoryBuffer,
    pub(super) edc_a: HistoryBuffer,
    pub(super) fclk_mhz: HistoryBuffer,
    pub(super) uclk_mhz: HistoryBuffer,
    pub(super) mclk_mhz: HistoryBuffer,
    pub(super) vddcr_soc_v: HistoryBuffer,
}

impl TelemetryHistory {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            ppt_w: HistoryBuffer::new(capacity),
            tctl_c: HistoryBuffer::new(capacity),
            tdc_a: HistoryBuffer::new(capacity),
            edc_a: HistoryBuffer::new(capacity),
            fclk_mhz: HistoryBuffer::new(capacity),
            uclk_mhz: HistoryBuffer::new(capacity),
            mclk_mhz: HistoryBuffer::new(capacity),
            vddcr_soc_v: HistoryBuffer::new(capacity),
        }
    }

    pub(super) fn record(&mut self, snap: &TelemetrySnapshot) {
        if let Some(v) = snap.ppt_value_w {
            self.ppt_w.push(f64::from(v));
        }
        if let Some(v) = snap.tctl_value_c {
            self.tctl_c.push(f64::from(v));
        }
        if let Some(v) = snap.tdc_value_a {
            self.tdc_a.push(f64::from(v));
        }
        if let Some(v) = snap.edc_value_a {
            self.edc_a.push(f64::from(v));
        }
        if let Some(v) = snap.fclk_mhz {
            self.fclk_mhz.push(f64::from(v));
        }
        if let Some(v) = snap.uclk_mhz {
            self.uclk_mhz.push(f64::from(v));
        }
        if let Some(v) = snap.mclk_mhz {
            self.mclk_mhz.push(f64::from(v));
        }
        if let Some(v) = snap.vddcr_soc_v {
            self.vddcr_soc_v.push(f64::from(v));
        }
    }
}
