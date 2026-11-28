use super::bench::parse_core_selection;
use super::info::render_manpage_bytes;
use zensmu_core::error::Error;

#[test]
fn parse_core_selection_supports_lists_ranges_and_deduplication() {
    assert_eq!(
        parse_core_selection("0,1,4..7", 8).unwrap(),
        vec![0, 1, 4, 5, 6, 7]
    );
    assert_eq!(
        parse_core_selection("2..=4,3,0-1", 8).unwrap(),
        vec![2, 3, 4, 0, 1]
    );
    assert_eq!(parse_core_selection(" 5 ", 8).unwrap(), vec![5]);

    assert!(matches!(
        parse_core_selection("8", 8),
        Err(Error::OutOfBounds { .. })
    ));
    assert!(matches!(
        parse_core_selection("4..2", 8),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        parse_core_selection("", 8),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        parse_core_selection("0,,2", 8),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        parse_core_selection("abc", 8),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn render_manpage_produces_valid_roff_output() {
    let bytes = render_manpage_bytes().unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains(".TH zensmu 1"), "{text}");
    assert!(text.contains("test"), "{text}");
    assert!(text.contains("export"), "{text}");
    assert!(text.contains("manpage"), "{text}");
}
