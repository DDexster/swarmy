//! Big three-row numerals for headline figures (`$612.40`, `94%`, `12`), drawn with half blocks.

/// Glyphs, three rows each. Digits are three cells wide; punctuation is narrower.
fn glyph(c: char) -> Option<[&'static str; 3]> {
    Some(match c {
        '0' => ["█▀█", "█ █", "▀▀▀"],
        '1' => ["▀█ ", " █ ", "▀▀▀"],
        '2' => ["▀▀█", "█▀▀", "▀▀▀"],
        '3' => ["▀▀█", " ▀█", "▀▀▀"],
        '4' => ["█ █", "▀▀█", "  ▀"],
        '5' => ["█▀▀", "▀▀█", "▀▀▀"],
        '6' => ["█▀▀", "█▀█", "▀▀▀"],
        '7' => ["▀▀█", "  █", "  ▀"],
        '8' => ["█▀█", "█▀█", "▀▀▀"],
        '9' => ["█▀█", "▀▀█", "▀▀▀"],
        '$' => ["▄█▀", "▀█▄", "▀▀ "],
        '%' => ["█ ▄", " ▄▀", "▀ ▀"],
        '.' | ',' => [" ", " ", "▀"],
        ':' => ["▄", "▄", " "],
        '-' => ["   ", "▀▀▀", "   "],
        '/' => ["  ▄", " ▄▀", "▀  "],
        'k' => ["█ ▄", "██ ", "▀ ▀"],
        'M' => ["█▄█", "█ █", "▀ ▀"],
        'h' => ["█  ", "█▀█", "▀ ▀"],
        'd' => ["  █", "█▀█", "▀▀▀"],
        'm' => ["   ", "█▀▀█", "▀  ▀"],
        ' ' => [" ", " ", " "],
        _ => return None,
    })
}

/// The three rows of `text` in big numerals; characters without a glyph are skipped.
/// Glyphs are separated by one blank column.
pub fn render(text: &str) -> [String; 3] {
    let mut rows = [String::new(), String::new(), String::new()];
    let mut first = true;
    for c in text.chars() {
        let Some(g) = glyph(c) else { continue };
        // The decimal point gets a blank column on both sides, or "9.72" reads as "972".
        let narrow = matches!(c, ',' | ':' | ' ');
        if !first && !narrow {
            for r in rows.iter_mut() {
                r.push(' ');
            }
        }
        // Pad each glyph to its widest row so rows stay aligned.
        let w = g.iter().map(|s| s.chars().count()).max().unwrap_or(0);
        for (r, part) in rows.iter_mut().zip(g) {
            r.push_str(part);
            for _ in part.chars().count()..w {
                r.push(' ');
            }
        }
        first = false;
    }
    rows
}

/// Display width of `text` in big numerals.
pub fn width(text: &str) -> usize {
    render(text)[0].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_align_and_widths_add_up() {
        let r = render("$12.5");
        assert!(
            r.iter()
                .all(|row| row.chars().count() == r[0].chars().count()),
            "{r:?}"
        );
        // $ 1 2 are 3 wide with gaps (3+1+3+1+3); '.' is 1 wide with a gap on each side; 5 is 3.
        assert_eq!(width("$12.5"), 3 + 1 + 3 + 1 + 3 + 1 + 1 + 1 + 3);
        assert_eq!(render("7")[2], "  ▀");
        assert_eq!(width("x"), 0);
    }
}
