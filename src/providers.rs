//! Stream candidates and quality selection.

#[derive(Debug, Clone)]
pub struct Stream {
    pub height: u32, // 0 == unknown
    pub url: String,
    pub referer: String,
    pub provider: String,
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
            if let Ok(want) = q.parse::<u32>() {
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
