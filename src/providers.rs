//! Stream candidates and quality selection.

#[derive(Debug, Clone)]
pub struct Stream {
    pub height: u32, // 0 == unknown
    pub url: String,
    pub referer: String,
    pub provider: String,
    /// Default subtitle track from the embed, if any (downloaded as a `.vtt` sidecar).
    pub subtitle: Option<String>,
}

/// "720", "720p", "1080P" → height. ani-cli documents `-q 720p`; a trailing p
/// is the unit, not part of the number.
pub fn parse_quality_height(quality: &str) -> Option<u32> {
    let q = quality.trim();
    let q = q.strip_suffix(['p', 'P']).unwrap_or(q).trim();
    q.parse().ok().filter(|&n| n > 0)
}

/// Pick a stream for the requested quality ("best", "worst", or a height).
pub fn select_quality<'a>(streams: &'a [Stream], quality: &str) -> Option<&'a Stream> {
    if streams.is_empty() {
        return None;
    }
    let mut ordered: Vec<&Stream> = streams.iter().collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.height));

    match quality {
        "best" => Some(ordered[0]),
        "worst" => ordered
            .iter()
            .rev()
            .find(|s| s.height > 0)
            .copied()
            .or_else(|| ordered.last().copied()),
        q => {
            if let Some(want) = parse_quality_height(q) {
                if let Some(exact) = ordered.iter().find(|s| s.height == want) {
                    return Some(exact);
                }
                if let Some(below) = ordered.iter().find(|s| s.height > 0 && s.height <= want) {
                    return Some(below);
                }
                eprintln!("  ! quality {q} not found, using best");
            }
            Some(ordered[0])
        }
    }
}
