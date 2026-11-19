use cytiq_transform::parse_channel_list as parse_transform_channel_list;
use std::io;
use std::path::PathBuf;

pub(crate) fn parse_threads(raw: &str) -> Result<usize, String> {
    let threads = raw
        .parse::<usize>()
        .map_err(|error| format!("invalid thread count `{raw}`: {error}"))?;
    if threads == 0 {
        Err("thread count must be greater than zero".to_owned())
    } else {
        Ok(threads)
    }
}

/// `cytoscan --stability-seeds`: 2 to [`cytiq_cytoscan::MAX_STABILITY_SEEDS`].
pub(crate) fn parse_stability_seeds(raw: &str) -> Result<usize, String> {
    let max = cytiq_cytoscan::MAX_STABILITY_SEEDS;
    match raw.parse::<usize>() {
        Ok(seeds) if (2..=max).contains(&seeds) => Ok(seeds),
        _ => Err(format!(
            "expected a seed count from 2 to {max}, got `{raw}`"
        )),
    }
}

pub(crate) fn parse_fraction(raw: &str) -> Result<f64, String> {
    match raw.parse::<f64>() {
        Ok(value) if value > 0.0 && value <= 1.0 => Ok(value),
        _ => Err(format!("expected a number in (0, 1], got `{raw}`")),
    }
}

pub(crate) fn parse_positive_usize(raw: &str) -> Result<usize, String> {
    match raw.parse::<usize>() {
        Ok(value) if value > 0 => Ok(value),
        _ => Err(format!("expected a positive integer, got `{raw}`")),
    }
}

/// A byte count: an integer with an optional unit `B`, `KiB`, `MiB`, `GiB`
/// or `TiB` (powers of 1024), greater than zero.
pub(crate) fn parse_byte_size(raw: &str) -> Result<u64, String> {
    let text = raw.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = (&text[..split], text[split..].trim());
    let multiplier: u64 = match unit {
        "" | "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        _ => {
            return Err(format!(
                "unknown unit `{unit}` in `{raw}`; use B, KiB, MiB, GiB or TiB"
            ));
        }
    };
    let value = digits
        .parse::<u64>()
        .map_err(|_| format!("expected a byte size such as `512MiB`, got `{raw}`"))?;
    match value.checked_mul(multiplier) {
        Some(0) => Err("byte size must be greater than zero".to_owned()),
        Some(bytes) => Ok(bytes),
        None => Err(format!("byte size `{raw}` overflows")),
    }
}

pub(crate) fn parse_positive_f64(raw: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|error| format!("invalid number `{raw}`: {error}"))?;
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err("value must be finite and greater than zero".to_owned())
    }
}

pub(crate) fn parse_finite_f64(raw: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|error| format!("invalid number `{raw}`: {error}"))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err("value must be finite".to_owned())
    }
}

pub(crate) fn parse_non_negative_f64(raw: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|error| format!("invalid number `{raw}`: {error}"))?;
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err("value must be finite and greater than or equal to zero".to_owned())
    }
}

/// Channel lists are checked in the command arm, not by clap, so that an
/// invalid list still writes `--outcome` (exit 2, invalid input).
fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

pub(crate) fn parse_optional_transform_channels(raw: Option<&str>) -> io::Result<Vec<String>> {
    raw.map(parse_transform_channel_list)
        .transpose()
        .map_err(|error| invalid_input(error.to_string()))
        .map(Option::unwrap_or_default)
}

/// `--channels` (comma-separated) followed by every `--channel`; at least one
/// channel is required.
pub(crate) fn parse_channels(
    channels: Option<&str>,
    repeated_channels: &[String],
) -> io::Result<Vec<String>> {
    let mut parsed = channels
        .map(cytiq_fcs::parse_channel_list)
        .transpose()
        .map_err(invalid_input)?
        .unwrap_or_default();
    for channel in repeated_channels {
        let channel = channel.trim();
        if channel.is_empty() {
            return Err(invalid_input("channel must not be empty"));
        }
        parsed.push(channel.to_owned());
    }
    if parsed.is_empty() {
        Err(invalid_input(
            "at least one `--channels` or `--channel` value is required",
        ))
    } else {
        Ok(parsed)
    }
}

/// A comma-separated `--channels` value, as [`parse_channels`] reports errors.
pub(crate) fn parse_channel_list(raw: &str) -> io::Result<Vec<String>> {
    cytiq_fcs::parse_channel_list(raw).map_err(invalid_input)
}

/// `LO,HI`: two finite numbers with `LO < HI`.
pub(crate) fn parse_range(raw: &str) -> Result<(f64, f64), String> {
    let invalid = || format!("expected `LO,HI` with finite LO < HI, got `{raw}`");
    let (lo, hi) = raw.split_once(',').ok_or_else(invalid)?;
    let lo = lo.trim().parse::<f64>().map_err(|_| invalid())?;
    let hi = hi.trim().parse::<f64>().map_err(|_| invalid())?;
    if lo.is_finite() && hi.is_finite() && lo < hi {
        Ok((lo, hi))
    } else {
        Err(invalid())
    }
}

fn parse_bounded_usize(raw: &str, min: usize, max: usize) -> Result<usize, String> {
    match raw.parse::<usize>() {
        Ok(value) if (min..=max).contains(&value) => Ok(value),
        _ => Err(format!(
            "expected an integer from {min} to {max}, got `{raw}`"
        )),
    }
}

pub(crate) fn parse_bins(raw: &str) -> Result<usize, String> {
    parse_bounded_usize(raw, cytiq_plot::MIN_BINS, cytiq_plot::MAX_BINS)
}

pub(crate) fn parse_max_points(raw: &str) -> Result<usize, String> {
    parse_bounded_usize(raw, 1, cytiq_plot::MAX_POINTS_LIMIT)
}

pub(crate) fn parse_plot_size(raw: &str) -> Result<u32, String> {
    match raw.parse::<u32>() {
        Ok(value) if (cytiq_plot::MIN_SIZE..=cytiq_plot::MAX_SIZE).contains(&value) => Ok(value),
        _ => Err(format!(
            "expected a size from {} to {} pixels, got `{raw}`",
            cytiq_plot::MIN_SIZE,
            cytiq_plot::MAX_SIZE
        )),
    }
}

pub(crate) fn parse_contour_step(raw: &str) -> Result<f64, String> {
    match raw.parse::<f64>() {
        Ok(value) if value > 0.0 && value <= cytiq_plot::MAX_CONTOUR_STEP => Ok(value),
        _ => Err(format!(
            "expected a number in (0, {}], got `{raw}`",
            cytiq_plot::MAX_CONTOUR_STEP
        )),
    }
}

/// `--outcome`: a file path; stdout (`-`) is reserved for command output.
pub(crate) fn parse_outcome_path(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() || raw == "-" {
        Err("`--outcome` needs a file path; stdout is reserved for command output".to_owned())
    } else {
        Ok(PathBuf::from(raw))
    }
}
