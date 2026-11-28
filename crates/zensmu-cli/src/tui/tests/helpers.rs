use super::super::format::{
    adjust_interval_ms, clamp_interval_ms, format_clock_ratio, format_co_bar, format_gauge_readout,
    format_linux_cpus, format_opt_f32, format_opt_u32, format_signed_margin, gauge_percent,
    gauge_ratio, ratio_color, thermal_color,
};
use super::super::history::HistoryBuffer;
use super::super::palette;
use super::super::{MAX_INTERVAL_MS, MIN_INTERVAL_MS};

#[test]
fn history_buffer_ring_eviction_and_stats() {
    let mut buf = HistoryBuffer::new(4);
    assert!(buf.is_empty());
    assert_eq!(buf.stats(), None);

    buf.push(10.0);
    buf.push(20.0);
    buf.push(f64::NAN); // ignored
    buf.push(f64::INFINITY); // ignored
    buf.push(30.0);
    buf.push(40.0);
    assert_eq!(buf.len(), 4);
    assert_eq!(buf.latest(), Some(40.0));
    assert_eq!(buf.min(), Some(10.0));
    assert_eq!(buf.max(), Some(40.0));
    assert_eq!(buf.avg(), Some(25.0));

    // Pushing a 5th value evicts 10.0
    buf.push(50.0);
    assert_eq!(buf.len(), 4);
    assert_eq!(buf.min(), Some(20.0));
    assert_eq!(buf.max(), Some(50.0));
    assert_eq!(buf.avg(), Some(35.0));

    // Window slicing returns the MOST RECENT samples
    assert_eq!(buf.sparkline_window(2, 10.0), vec![400, 500]);
    assert_eq!(buf.sparkline_window(10, 1.0), vec![20, 30, 40, 50]);
}

#[test]
fn gauge_ratio_percent_and_color_thresholds() {
    assert!((gauge_ratio(Some(44.0), Some(88.0)) - 0.5).abs() < 1e-6);
    assert_eq!(gauge_percent(Some(44.0), Some(88.0)), Some(50));
    assert_eq!(gauge_ratio(Some(120.0), Some(88.0)), 1.0);
    assert_eq!(gauge_ratio(None, Some(88.0)), 0.0);
    assert_eq!(gauge_ratio(Some(44.0), Some(0.0)), 0.0);
    assert_eq!(gauge_percent(Some(44.0), None), None);

    assert_eq!(ratio_color(0.40), palette::GREEN);
    assert_eq!(ratio_color(0.75), palette::YELLOW);
    assert_eq!(ratio_color(0.92), palette::RED);

    assert_eq!(thermal_color(Some(50.0), Some(95.0)), palette::GREEN);
    assert_eq!(thermal_color(Some(68.0), Some(95.0)), palette::YELLOW);
    assert_eq!(thermal_color(Some(78.0), Some(95.0)), palette::ORANGE);
    assert_eq!(thermal_color(Some(88.0), Some(95.0)), palette::RED);
}

#[test]
fn formatting_and_interval_helpers() {
    assert_eq!(clamp_interval_ms(10), MIN_INTERVAL_MS);
    assert_eq!(clamp_interval_ms(50_000), MAX_INTERVAL_MS);
    assert_eq!(adjust_interval_ms(500, true), 600);
    assert_eq!(adjust_interval_ms(500, false), 400);
    assert_eq!(adjust_interval_ms(100, false), 100);
    assert_eq!(adjust_interval_ms(1000, true), 1250);
    assert_eq!(adjust_interval_ms(2000, true), 2500);

    assert_eq!(format_signed_margin(-20), "-20");
    assert_eq!(format_signed_margin(0), "0");
    assert_eq!(format_signed_margin(5), "+5");

    assert_eq!(format_opt_f32(Some(1.24), 3, "V"), "1.240 V");
    assert_eq!(format_opt_f32(None, 3, "V"), "—");
    assert_eq!(format_opt_u32(Some(5550), "MHz"), "5550 MHz");
    assert_eq!(format_opt_u32(None, "MHz"), "—");

    assert_eq!(
        format_gauge_readout(Some(42.5), Some(88.0), "W"),
        " 42.5 /  88.0 W ( 48%)"
    );
    assert_eq!(
        format_clock_ratio(Some(3000.0), Some(3000.0)),
        Some("1:1 sync")
    );
    assert_eq!(
        format_clock_ratio(Some(1500.0), Some(3000.0)),
        Some("1:2 div")
    );
    assert_eq!(format_linux_cpus(&[0, 8]), "0,8");
    assert_eq!(format_linux_cpus(&[]), "—");

    let bar_neg = format_co_bar(-25, 50, 4);
    assert_eq!(bar_neg, "[  ██│    ]");
    let bar_pos = format_co_bar(50, 50, 4);
    assert_eq!(bar_pos, "[    │████]");
    let bar_zero = format_co_bar(0, 50, 4);
    assert_eq!(bar_zero, "[    │    ]");
}
