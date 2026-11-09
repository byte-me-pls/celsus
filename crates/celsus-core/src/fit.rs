//! Fitting translated text into the box the original text occupied.
//!
//! Translations are often longer than the source. The text is first wrapped at the original size; if it does not
//! fit, the size is reduced step by step down to a floor. Below the floor the block is reported instead of being
//! squeezed into something unreadable.

/// Measures text in a given font. Widths are in points.
pub trait Measure {
    fn width(&self, text: &str, size: f32) -> f32;
    /// Ascender as a fraction of the font size.
    fn ascent(&self) -> f32;
    /// Descender as a positive fraction of the font size.
    fn descent(&self) -> f32;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fitted {
    pub size: f32,
    pub lines: Vec<String>,
    /// Distance between baselines in points.
    pub leading: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct FitOptions {
    /// Smallest allowed size as a fraction of the original size.
    pub min_scale: f32,
    /// Absolute smallest size in points.
    pub min_size: f32,
    /// Baseline distance as a multiple of the size, used when the original leading is unknown.
    pub line_height: f32,
    pub step: f32,
}

impl Default for FitOptions {
    fn default() -> Self {
        Self {
            min_scale: 0.62,
            min_size: 5.0,
            line_height: 1.15,
            step: 0.25,
        }
    }
}

/// Wrap `text` into lines no wider than `width` at `size`. Words longer than a line are broken by character.
pub fn wrap(text: &str, width: f32, size: f32, m: &dyn Measure) -> Vec<String> {
    wrap_indented(text, width, Indent::default(), size, m)
}

/// Paragraph indentation in points: of the first line and of the lines after it (a hanging indent).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Indent {
    pub first: f32,
    pub rest: f32,
}

impl Indent {
    pub fn of_line(&self, n: usize) -> f32 {
        if n == 0 { self.first } else { self.rest }
    }
}

/// [`wrap`] with indented lines, which are that much narrower.
pub fn wrap_indented(
    text: &str,
    full_width: f32,
    indent: Indent,
    size: f32,
    m: &dyn Measure,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let width_at = |n: usize| full_width - indent.of_line(n);
    for paragraph in text.split('\n') {
        // Runs of spaces count as one; then the Unicode line breaking rules (UAX #14) say where a line may end:
        // after spaces, and between the characters of Chinese and Japanese, never before closing punctuation.
        let paragraph = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut line = String::new();
        let mut from = 0;
        for (to, _) in unicode_linebreak::linebreaks(&paragraph) {
            let piece = &paragraph[from..to];
            from = to;
            let candidate = format!("{line}{piece}");
            if m.width(candidate.trim_end(), size) <= width_at(lines.len()) {
                line = candidate;
                continue;
            }
            if !line.trim().is_empty() {
                lines.push(line.trim_end().to_owned());
            }
            line = String::new();
            let word = piece.trim_end();
            if m.width(word, size) <= width_at(lines.len()) {
                line = piece.to_owned();
            } else {
                // Longer than a line: break between characters, but never just before a combining mark.
                for ch in word.chars() {
                    let mut next = line.clone();
                    next.push(ch);
                    if !line.is_empty()
                        && !is_mark(ch)
                        && m.width(&next, size) > width_at(lines.len())
                    {
                        lines.push(std::mem::take(&mut line));
                        line.push(ch);
                    } else {
                        line = next;
                    }
                }
                if piece.ends_with(' ') {
                    line.push(' ');
                }
            }
        }
        if !line.trim().is_empty() {
            lines.push(line.trim_end().to_owned());
        }
    }
    lines
}

/// Characters that belong to the one before them (accents, vowel signs) and must not start a line.
fn is_mark(c: char) -> bool {
    use unicode_linebreak::{BreakClass, break_property};
    matches!(
        break_property(c as u32),
        BreakClass::CombiningMark | BreakClass::ZeroWidthJoiner
    ) || matches!(c,
        '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}' // Thai
        | '\u{0EB1}' | '\u{0EB4}'..='\u{0EBC}' | '\u{0EC8}'..='\u{0ECD}' // Lao
        | '\u{17B6}'..='\u{17D3}' // Khmer
        | '\u{102B}'..='\u{103E}' | '\u{1056}'..='\u{1059}' // Myanmar
    )
}

/// The space a translation may use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub width: f32,
    pub height: f32,
    pub indent: Indent,
    /// Height of one line as a fraction of the size, taken from the original text's line box. `None` uses the
    /// font's ascent plus descent.
    pub line_box: Option<f32>,
}

/// Fit `text` into a `width` x `height` box, starting at `size`. `leading` is the original baseline distance, if
/// the block had more than one line; it scales with the size.
pub fn fit(
    text: &str,
    area: Area,
    size: f32,
    leading: Option<f32>,
    m: &dyn Measure,
    opts: FitOptions,
) -> Option<Fitted> {
    let Area {
        width,
        height,
        indent,
        line_box,
    } = area;
    let line_box = line_box.unwrap_or(m.ascent() + m.descent());
    if text.trim().is_empty() || width <= 0.0 || height <= 0.0 || size <= 0.0 {
        return None;
    }
    let ratio = leading
        .map(|l| l / size)
        .filter(|r| (0.9..=2.5).contains(r))
        .unwrap_or(opts.line_height);
    let floor = (size * opts.min_scale).max(opts.min_size).min(size);
    // Text given as separate lines (a list, table of contents entries) keeps one line per entry: a smaller size
    // is tried before any entry is broken.
    let entries = text.split('\n').filter(|l| !l.trim().is_empty()).count();
    if entries > 1 {
        let mut s = size;
        loop {
            let lines = wrap_indented(text, width, indent, s, m);
            let used = s * line_box + s * ratio * (lines.len().saturating_sub(1)) as f32;
            if lines.len() == entries && used <= height + 0.2 {
                return Some(Fitted {
                    size: s,
                    lines,
                    leading: s * ratio,
                });
            }
            if s <= floor + f32::EPSILON {
                break;
            }
            s = (s - opts.step).max(floor);
        }
    }
    let mut s = size;
    loop {
        let lines = wrap_indented(text, width, indent, s, m);
        let fits_width = lines
            .iter()
            .enumerate()
            .all(|(n, l)| m.width(l, s) <= width - indent.of_line(n) + 0.01);
        // Tighter line spacing is tried before a smaller size, but never below 1.08.
        for r in [ratio, (ratio * 0.9).max(1.08).min(ratio)] {
            let lead = s * r;
            let used = s * line_box + lead * (lines.len().saturating_sub(1)) as f32;
            if !lines.is_empty() && fits_width && used <= height + 0.2 {
                return Some(Fitted {
                    size: s,
                    lines,
                    leading: lead,
                });
            }
        }
        if s <= floor + f32::EPSILON {
            return None;
        }
        s = (s - opts.step).max(floor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character is half the font size wide.
    struct Mono;
    impl Measure for Mono {
        fn width(&self, text: &str, size: f32) -> f32 {
            text.chars().count() as f32 * size * 0.5
        }
        fn ascent(&self) -> f32 {
            0.8
        }
        fn descent(&self) -> f32 {
            0.2
        }
    }

    #[test]
    fn wraps_on_words() {
        // 10pt -> 5pt per char, 50pt -> 10 chars per line.
        assert_eq!(
            wrap("bir iki üç dört beş", 50.0, 10.0, &Mono),
            vec!["bir iki üç", "dört beş"]
        );
    }

    #[test]
    fn indented_lines_are_narrower() {
        let indent = Indent {
            first: 10.0,
            rest: 0.0,
        };
        // First line has 40pt = 8 chars, the rest 50pt = 10 chars.
        assert_eq!(
            wrap_indented("bir iki üç dört beş", 50.0, indent, 10.0, &Mono),
            vec!["bir iki", "üç dört", "beş"]
        );
    }

    #[test]
    fn list_entries_shrink_before_breaking() {
        // The second entry is 14 chars = 70pt at 10pt in a 60pt box: it shrinks instead of wrapping.
        let area = Area {
            width: 60.0,
            height: 30.0,
            indent: Indent::default(),
            line_box: None,
        };
        let f = fit(
            "kısa\nuzun bir giriş",
            area,
            10.0,
            None,
            &Mono,
            FitOptions::default(),
        )
        .unwrap();
        assert_eq!(f.lines, vec!["kısa", "uzun bir giriş"]);
        assert!(f.size < 10.0 && f.size >= 6.2, "size {}", f.size);
    }

    #[test]
    fn breaks_words_longer_than_a_line() {
        assert_eq!(
            wrap("abcdefghijkl", 25.0, 10.0, &Mono),
            vec!["abcde", "fghij", "kl"]
        );
    }

    #[test]
    fn keeps_size_when_it_fits() {
        let f = fit(
            "kısa",
            Area {
                width: 100.0,
                height: 12.0,
                indent: Indent::default(),
                line_box: None,
            },
            10.0,
            None,
            &Mono,
            FitOptions::default(),
        )
        .unwrap();
        assert_eq!(f.size, 10.0);
        assert_eq!(f.lines, vec!["kısa"]);
    }

    #[test]
    fn shrinks_before_giving_up() {
        // 20 chars at 10pt is 100pt wide: one line needs a smaller size to fit 80pt.
        let f = fit(
            "yirmi karakterlik ab",
            Area {
                width: 80.0,
                height: 10.5,
                indent: Indent::default(),
                line_box: None,
            },
            10.0,
            None,
            &Mono,
            FitOptions::default(),
        )
        .unwrap();
        assert!(f.size < 10.0 && f.size >= 6.2, "size {}", f.size);
        assert_eq!(f.lines.len(), 1);
    }

    #[test]
    fn reports_when_even_the_floor_does_not_fit() {
        let long = "çok uzun bir çeviri metni ".repeat(20);
        assert!(
            fit(
                &long,
                Area {
                    width: 50.0,
                    height: 10.0,
                    indent: Indent::default(),
                    line_box: None
                },
                10.0,
                None,
                &Mono,
                FitOptions::default()
            )
            .is_none()
        );
    }
}
