//! Comma-separated channel lists, as every command's `--channels` takes them,
//! and which channels are scatter.

/// Split `raw` at commas into trimmed channel names, dropping empty ones.
///
/// # Errors
///
/// Returns `channel list must contain at least one channel` when no name is
/// left.
pub fn parse_channel_list(raw: &str) -> Result<Vec<String>, String> {
    let channels = raw
        .split(',')
        .map(str::trim)
        .filter(|channel| !channel.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if channels.is_empty() {
        Err("channel list must contain at least one channel".to_owned())
    } else {
        Ok(channels)
    }
}

/// Separators between the words of a `$PnN`.
const WORD_SEPARATORS: [char; 4] = [' ', '-', '_', '.'];

/// Whether `$PnN` names a light-scatter channel, whatever the instrument
/// calls it: `FSC…` and `SSC…` (`FSC-A`, `SSC-H`, `SSC-B-A`), a first word
/// `FS` or `SS` (Beckman Coulter Navios and Gallios: `FS INT`, `SS PEAK`,
/// `SS TOF`, which R's `make.names` turns into `FS.INT`), or an `FSC` or `SSC`
/// word anywhere (`Violet SSC-A`). Every command that treats scatter apart
/// from fluorescence asks this, so they agree on a file.
#[must_use]
pub fn is_scatter_channel(name: &str) -> bool {
    let upper = name.trim().to_ascii_uppercase();
    if upper.starts_with("FSC") || upper.starts_with("SSC") {
        return true;
    }
    let mut words = upper.split(WORD_SEPARATORS).filter(|word| !word.is_empty());
    matches!(words.next(), Some("FS" | "SS")) || words.any(|word| word == "FSC" || word == "SSC")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_separated_channels() {
        let channels = parse_channel_list("CD3, CD4,,CD8").expect("channels should parse");
        assert_eq!(channels, vec!["CD3", "CD4", "CD8"]);
    }

    #[test]
    fn rejects_empty_comma_separated_channels() {
        let error = parse_channel_list(" , ,, ").expect_err("empty list should fail");
        assert!(error.contains("at least one channel"));
    }

    #[test]
    fn recognises_scatter_channels_of_every_naming() {
        for name in [
            "FSC-A",
            "FSC-H",
            "FSC-W",
            "SSC-A",
            "ssc-h",
            "SSC-B-A",
            "FSC1-A",
            "FS INT",
            "FS PEAK",
            "SS INT",
            "SS PEAK",
            "SS TOF",
            "FS.INT",
            "SS.TOF",
            "Violet SSC-A",
            " fs int ",
        ] {
            assert!(is_scatter_channel(name), "{name}");
        }
        for name in [
            "FL1 INT",
            "FL1-A",
            "Time",
            "TIME",
            "BV421-A",
            "CD3",
            "Comp-FL1-A",
            "SSB-A",
            "FSX",
            "",
        ] {
            assert!(!is_scatter_channel(name), "{name}");
        }
    }

    #[test]
    fn trims_names_and_drops_empty_ones() {
        assert_eq!(
            parse_channel_list(" CD3, ,CD4 ,"),
            Ok(vec!["CD3".to_owned(), "CD4".to_owned()])
        );
    }

    #[test]
    fn rejects_a_list_without_names() {
        assert_eq!(
            parse_channel_list(" , "),
            Err("channel list must contain at least one channel".to_owned())
        );
    }
}
