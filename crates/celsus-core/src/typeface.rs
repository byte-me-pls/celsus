//! Finding an installed typeface close to the one a document is set in.
//!
//! The fonts inside a PDF hold only the letters of the original text, so a translation is written with fonts
//! from this machine. Which ones is decided here, once for a document: a family of the same name where it is
//! installed, a family drawn to the same widths where one is known (Arial for Helvetica), and otherwise the
//! installed family whose letters are nearest in width, so that the text runs about as long as it did and looks
//! like what it replaces.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use celsus_pdf::font::{Font, FontKind};
use celsus_pdf::ttf::{TrueType, faces_in};

use crate::fonts::{Family, font_index};
use crate::languages::Script;

/// The width of each letter at size 1: `a` to `z`, then `A` to `Z`. Nothing is known about a letter at 0.
type Widths = [f32; 52];

/// Regular, italic, bold, bold italic.
const SLOTS: usize = 4;

/// Fewer letters than this say too little about a design to compare it with another.
const FEW_LETTERS: usize = 12;

/// A family the document already has a stand-in from is used again unless another is nearer by more than
/// this, so that a book is not set in five faces that differ by a hair.
const KEPT: f32 = 0.012;

/// A design of which it is not known whether it has serifs is taken for one without, unless a face with serifs
/// is nearer by this much: widths alone tell the two kinds apart most of the time, not always.
const SERIF_MARGIN: f32 = 0.8;

/// How often each letter comes up in running text, in percent, with a floor for the rare ones.
const OFTEN: [f32; 26] = [
    8.2, 1.5, 2.8, 4.3, 12.7, 2.2, 2.0, 6.1, 7.0, 1.0, 1.0, 4.0, 2.4, 6.7, 7.5, 1.9, 1.0, 6.0, 6.3, 9.1,
    2.8, 1.0, 2.4, 1.0, 2.0, 1.0,
];
const CAPITAL: f32 = 0.3;

/// Families drawn to the same widths: text set in one keeps its line breaks in another.
const SAME_WIDTHS: &[&[&str]] = &[
    &["Arial", "Helvetica", "Helvetica Neue", "Liberation Sans", "Arimo", "Nimbus Sans"],
    &["Times New Roman", "Times", "Liberation Serif", "Tinos", "Nimbus Roman"],
    &["Courier New", "Courier", "Liberation Mono", "Cousine", "Nimbus Mono"],
    &["Palatino Linotype", "Palatino", "Book Antiqua", "URW Palladio"],
    &["Calibri", "Carlito"],
    &["Cambria", "Caladea"],
];

/// What a family is known by, whatever the face and however the name is written: `TimesNewRomanPS-BoldMT`,
/// `Times New Roman,Bold` and `Times New Roman` are one family.
pub(crate) fn family_key(name: &str) -> String {
    const ENDINGS: &[&str] = &[
        "bolditalic", "boldoblique", "bold", "italic", "oblique", "regular", "roman", "medium", "semibold",
        "demibold", "demi", "light", "book", "psmt", "mt", "ps", "std", "pro", "lt", "com",
    ];
    let family = name.split(['-', ',']).next().unwrap_or(name);
    let mut key: String = family
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    while let Some(left) = ENDINGS
        .iter()
        .find_map(|e| key.strip_suffix(e))
        .map(str::len)
        .filter(|&left| left > 0)
    {
        key.truncate(left);
    }
    key
}

fn letter(c: char) -> Option<usize> {
    match c {
        'a'..='z' => Some(c as usize - 'a' as usize),
        'A'..='Z' => Some(26 + c as usize - 'A' as usize),
        _ => None,
    }
}

/// How far apart two designs are: the difference in width of the letters both have, the common letters
/// counting for more.
fn distance(a: &Widths, b: &Widths) -> Option<f32> {
    let (mut sum, mut total, mut letters) = (0.0, 0.0, 0);
    for k in 0..a.len() {
        if a[k] <= 0.0 || b[k] <= 0.0 {
            continue;
        }
        let weight = if k < 26 { OFTEN[k] } else { CAPITAL };
        sum += weight * (a[k] - b[k]).abs();
        total += weight;
        letters += 1;
    }
    (letters >= FEW_LETTERS).then(|| sum / total)
}

/// One face of an installed family.
#[derive(Debug, Clone)]
struct Installed {
    path: PathBuf,
    index: u32,
    postscript: String,
    weight: u16,
    panose: [u8; 10],
    ranges: u32,
}

impl Installed {
    fn widths(&self) -> Option<Widths> {
        let data = std::fs::read(&self.path).ok()?;
        let font = TrueType::parse_shared(Arc::new(data), self.index).ok()?;
        let em = f32::from(font.units_per_em.max(1));
        let mut widths = [0.0; 52];
        for c in ('a'..='z').chain('A'..='Z') {
            if let (Some(k), Some(glyph)) = (letter(c), font.glyph(c)) {
                widths[k] = f32::from(font.advance(glyph)) / em;
            }
        }
        Some(widths)
    }
}

/// An installed family a text can be set in: of a text design, with outlines that can be read here and a
/// licence that lets them go into a document.
#[derive(Debug)]
struct InstalledFamily {
    key: String,
    class: Family,
    faces: [Option<Installed>; SLOTS],
    /// The Unicode ranges all its faces cover.
    covers: u32,
    /// Measured when the family is first compared with a document's font.
    widths: OnceLock<[Option<Widths>; SLOTS]>,
}

impl InstalledFamily {
    /// The widths of a face; of the regular face where the family has no such face.
    fn widths(&self, slot: usize) -> Option<&Widths> {
        let all = self
            .widths
            .get_or_init(|| std::array::from_fn(|s| self.faces[s].as_ref()?.widths()));
        all[slot].as_ref().or(all[0].as_ref())
    }
}

/// The text families of a fonts folder. Looked through once for as long as the program runs: it reads a few
/// hundred bytes of every font file.
fn catalogue(dir: &Path) -> Arc<Vec<InstalledFamily>> {
    static SEEN: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<InstalledFamily>>>>> = OnceLock::new();
    let mut seen = SEEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(known) = seen.get(dir) {
        return known.clone();
    }
    let found = Arc::new(look_through(dir));
    seen.insert(dir.to_path_buf(), found.clone());
    found
}

fn look_through(dir: &Path) -> Vec<InstalledFamily> {
    let mut files: Vec<PathBuf> = font_index(dir).into_values().collect();
    files.sort();
    let mut families: BTreeMap<String, [Option<Installed>; SLOTS]> = BTreeMap::new();
    for path in files {
        let Ok(mut file) = std::fs::File::open(&path) else {
            continue;
        };
        for face in faces_in(&mut file) {
            // A text design (PANOSE kind 2), not one for Chinese, Japanese or Korean, which names its Latin
            // letters a text design too.
            let wanted = face.truetype
                && face.embeddable
                && face.panose[0] == 2
                && face.ranges[1] & (1 << 27) == 0;
            let key = family_key(&face.family);
            if !wanted || key.is_empty() {
                continue;
            }
            let bold = face.weight >= 600;
            let slot = usize::from(bold) * 2 + usize::from(face.italic);
            let off = |weight: u16| weight.abs_diff(if bold { 700 } else { 400 });
            let held = &mut families.entry(key).or_default()[slot];
            if held.as_ref().is_none_or(|h| off(face.weight) < off(h.weight)) {
                *held = Some(Installed {
                    path: path.clone(),
                    index: face.index,
                    postscript: face.postscript,
                    weight: face.weight,
                    panose: face.panose,
                    ranges: face.ranges[0],
                });
            }
        }
    }
    families
        .into_iter()
        .filter_map(|(key, faces)| {
            let regular = faces[0].as_ref()?;
            let class = match (regular.panose[3], regular.panose[1]) {
                (9, _) => Family::Mono,
                (_, 2..=10) => Family::Serif,
                (_, 11..=15) => Family::Sans,
                _ => return None,
            };
            // Listings get by with one face. Running text needs all four: a family without an italic would
            // print its emphasis upright.
            if class != Family::Mono && faces.iter().any(Option::is_none) {
                return None;
            }
            let covers = faces.iter().flatten().fold(u32::MAX, |all, f| all & f.ranges);
            Some(InstalledFamily {
                key,
                class,
                faces,
                covers,
                widths: OnceLock::new(),
            })
        })
        .collect()
}

/// What a document shows of one family.
struct Seen {
    widths: [Widths; SLOTS],
    letters: [usize; SLOTS],
    class: Option<Family>,
    /// The letters of the font the class was read from: the fullest one is believed.
    most: usize,
}

impl Default for Seen {
    fn default() -> Self {
        Seen {
            widths: [[0.0; 52]; SLOTS],
            letters: [0; SLOTS],
            class: None,
            most: 0,
        }
    }
}

impl Seen {
    fn add(&mut self, font: &Font) {
        let slot = usize::from(font.bold) * 2 + usize::from(font.italic);
        let mut letters = 0;
        for &(c, width) in &font.letters {
            let Some(k) = letter(c) else { continue };
            letters += 1;
            if self.widths[slot][k] <= 0.0 && width > 0.0 {
                self.widths[slot][k] = width;
                self.letters[slot] += 1;
            }
        }
        if letters >= self.most {
            self.most = letters.max(1);
            self.class = class_of(font);
        }
    }

    /// How far the family is from an installed one: the mean over the faces the document shows enough of.
    fn distance(&self, family: &InstalledFamily) -> Option<f32> {
        let (mut sum, mut faces) = (0.0, 0);
        for slot in 0..SLOTS {
            if self.letters[slot] < FEW_LETTERS {
                continue;
            }
            if let Some(d) = family.widths(slot).and_then(|w| distance(&self.widths[slot], w)) {
                sum += d;
                faces += 1;
            }
        }
        (faces > 0).then(|| sum / faces as f32)
    }
}

/// The kind of design a document's font is, where the file says: fixed pitch, with serifs, or without them by
/// its name. Many files say nothing.
fn class_of(font: &Font) -> Option<Family> {
    const SANS: &[&str] = &[
        "sans", "gothic", "grotesk", "grotesque", "helvet", "arial", "verdana", "futura", "univers",
        "frutiger", "myriad", "calibri", "tahoma", "segoe", "roboto", "lato", "avenir", "optima", "trebuchet",
    ];
    let name = font.name.to_ascii_lowercase();
    if font.fixed_pitch {
        Some(Family::Mono)
    } else if font.serif {
        Some(Family::Serif)
    } else if SANS.iter().any(|s| name.contains(s)) {
        Some(Family::Sans)
    } else {
        None
    }
}

/// A face to load for a document.
#[derive(Debug, Clone)]
pub(crate) struct Pick {
    pub path: PathBuf,
    pub index: u32,
    pub postscript: String,
    pub bold: bool,
    pub italic: bool,
}

/// The installed family that stands in for one of a document's.
#[derive(Debug, Clone)]
pub(crate) struct Choice {
    /// The document's family, as [`family_key`] names it.
    pub key: String,
    pub class: Family,
    /// The regular face first.
    pub faces: Vec<Pick>,
}

/// An installed family for each family of fonts a document uses, the document's fullest families first. Only
/// for text in Latin, Cyrillic or Greek letters: the other writing systems have their own short lists of fonts.
/// The answer depends on the document and the folder alone, so a book rendered in parts comes out in one face.
pub(crate) fn choose<'a>(
    dir: &Path,
    script: Option<Script>,
    fonts: impl IntoIterator<Item = &'a Font>,
) -> Vec<Choice> {
    // Bits of the first Unicode range word: Basic Latin, Latin-1, Latin Extended-A, Greek, Cyrillic.
    let needs: u32 = match script {
        None | Some(Script::Latn) => 0b111,
        Some(Script::Grek) => 1 | 1 << 7,
        Some(Script::Cyrl) => 1 | 1 << 9,
        _ => return Vec::new(),
    };
    let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
    for font in fonts {
        // Type 3 fonts are drawings with a made-up name, and upright text has its own fonts.
        if font.kind == FontKind::Type3 || font.vertical {
            continue;
        }
        let key = family_key(&font.name);
        if !key.is_empty() {
            seen.entry(key).or_default().add(font);
        }
    }
    if seen.is_empty() {
        return Vec::new();
    }
    let installed = catalogue(dir);
    let usable: Vec<&InstalledFamily> = installed
        .iter()
        .filter(|f| f.covers & needs == needs)
        .collect();
    // The families with a regular face and many letters settle first, and the thin ones follow them.
    let mut order: Vec<(&String, &Seen)> = seen.iter().collect();
    order.sort_by_key(|(_, s)| (s.letters[0] == 0, Reverse(s.letters.iter().sum::<usize>())));
    let mut chosen: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    for (key, seen) in order {
        let pick = usable
            .iter()
            .position(|f| f.key == *key)
            .or_else(|| same_widths(key, &usable))
            .or_else(|| nearest(seen, &usable, &chosen));
        let Some(pick) = pick else { continue };
        if !chosen.contains(&pick) {
            chosen.push(pick);
        }
        let family = usable[pick];
        out.push(Choice {
            key: key.clone(),
            class: family.class,
            faces: family
                .faces
                .iter()
                .enumerate()
                .filter_map(|(slot, face)| {
                    let face = face.as_ref()?;
                    Some(Pick {
                        path: face.path.clone(),
                        index: face.index,
                        postscript: face.postscript.clone(),
                        bold: slot >= 2,
                        italic: slot % 2 == 1,
                    })
                })
                .collect(),
        });
    }
    out
}

/// An installed family drawn to the widths of the one asked for.
fn same_widths(key: &str, usable: &[&InstalledFamily]) -> Option<usize> {
    let group = SAME_WIDTHS
        .iter()
        .find(|g| g.iter().any(|name| family_key(name) == key))?;
    group
        .iter()
        .find_map(|name| usable.iter().position(|f| f.key == family_key(name)))
}

/// The installed family nearest in width, among those of the same kind of design.
fn nearest(seen: &Seen, usable: &[&InstalledFamily], chosen: &[usize]) -> Option<usize> {
    let best_of = |class: Family| -> Option<(usize, f32)> {
        let of_class = |i: &usize| usable[*i].class == class;
        let measured = |i: usize| Some((i, seen.distance(usable[i])?));
        let by_distance = |a: &(usize, f32), b: &(usize, f32)| a.1.total_cmp(&b.1);
        let best = (0..usable.len())
            .filter(of_class)
            .filter_map(measured)
            .min_by(by_distance)?;
        chosen
            .iter()
            .copied()
            .filter(of_class)
            .filter_map(measured)
            .filter(|(_, d)| *d <= best.1 + KEPT)
            .min_by(by_distance)
            .or(Some(best))
    };
    match seen.class {
        Some(class) => best_of(class).map(|(i, _)| i),
        None => match (best_of(Family::Sans), best_of(Family::Serif)) {
            (Some((sans, plain)), Some((serif, with))) => {
                Some(if with < plain * SERIF_MARGIN { serif } else { sans })
            }
            (sans, serif) => sans.or(serif).map(|(i, _)| i),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_family_is_known_by_one_name() {
        for name in [
            "Times New Roman",
            "TimesNewRomanPSMT",
            "TimesNewRomanPS-BoldItalicMT",
            "Times New Roman,Bold",
            "TimesNewRoman-Italic",
        ] {
            assert_eq!(family_key(name), family_key("Times New Roman"), "{name}");
        }
        assert_eq!(family_key("Arial-BoldMT"), family_key("ArialMT"));
        assert_eq!(family_key("SabonLTPro-Roman"), "sabon");
        assert_eq!(family_key("NewBaskervilleStd-Italic"), "newbaskerville");
        assert_eq!(family_key("Segoe UI"), family_key("SegoeUI-Bold"));
        assert_ne!(family_key("Arial"), family_key("Arial Black"));
        // A name that is nothing but a style stays what it is.
        assert_eq!(family_key("Bold"), "bold");
        assert_eq!(family_key(""), "");
    }

    #[test]
    fn designs_are_compared_by_the_letters_both_have() {
        let mut a = [0.0; 52];
        let mut b = [0.0; 52];
        a[..26].fill(0.5);
        b[..26].fill(0.5);
        assert_eq!(distance(&a, &b), Some(0.0));
        // A letter only one of them has says nothing.
        a[30] = 0.9;
        assert_eq!(distance(&a, &b), Some(0.0));
        // The common letters weigh more: `e` against `z`.
        let wider = |k: usize| {
            let mut c = b;
            c[k] += 0.1;
            distance(&a, &c).unwrap()
        };
        assert!(wider(4) > wider(25) * 5.0);
        // Too few letters to tell.
        let mut few = [0.0; 52];
        few[..5].fill(0.5);
        assert_eq!(distance(&few, &b), None);
    }

    /// An installed family whose letters are all of one width.
    fn installed(name: &str, class: Family, width: f32) -> InstalledFamily {
        let mut widths = [0.0; 52];
        widths[..26].fill(width);
        InstalledFamily {
            key: family_key(name),
            class,
            faces: [None, None, None, None],
            covers: u32::MAX,
            widths: OnceLock::from([Some(widths); SLOTS]),
        }
    }

    /// What a document shows of a family: a regular face with letters of one width.
    fn shown(class: Option<Family>, width: f32) -> Seen {
        let mut seen = Seen {
            class,
            ..Default::default()
        };
        seen.widths[0][..26].fill(width);
        seen.letters[0] = 26;
        seen
    }

    #[test]
    fn the_nearest_family_of_the_same_kind_stands_in() {
        let families = [
            installed("Plain", Family::Sans, 0.50),
            installed("Plain Wide", Family::Sans, 0.505),
            installed("Footed", Family::Serif, 0.45),
            installed("Typed", Family::Mono, 0.60),
        ];
        let usable: Vec<&InstalledFamily> = families.iter().collect();
        let pick = |seen: &Seen, chosen: &[usize]| {
            nearest(seen, &usable, chosen).map(|i| usable[i].key.as_str())
        };
        // The kind of design decides before the widths do.
        assert_eq!(pick(&shown(Some(Family::Serif), 0.50), &[]), Some("footed"));
        assert_eq!(pick(&shown(Some(Family::Sans), 0.45), &[]), Some("plain"));
        assert_eq!(pick(&shown(Some(Family::Mono), 0.50), &[]), Some("typed"));
        // Of a design that says nothing about itself the widths decide, and serifs only when they are
        // clearly nearer.
        assert_eq!(pick(&shown(None, 0.51), &[]), Some("plainwide"));
        assert_eq!(pick(&shown(None, 0.455), &[]), Some("footed"));
        assert_eq!(pick(&shown(None, 0.476), &[]), Some("plain"));
        // A family the document already uses is kept over one a hair nearer, not over one far nearer.
        assert_eq!(pick(&shown(Some(Family::Sans), 0.506), &[0]), Some("plain"));
        assert_eq!(pick(&shown(Some(Family::Sans), 0.506), &[2]), Some("plainwide"));
        // Nothing to compare: too few letters.
        let mut thin = shown(None, 0.5);
        thin.letters[0] = 5;
        assert_eq!(pick(&thin, &[]), None);
    }

    #[test]
    fn a_family_drawn_to_the_same_widths_stands_in() {
        let families = [
            installed("Georgia", Family::Serif, 0.5),
            installed("Liberation Sans", Family::Sans, 0.5),
            installed("Arial", Family::Sans, 0.5),
        ];
        let usable: Vec<&InstalledFamily> = families.iter().collect();
        let stand_in = |name: &str| {
            same_widths(&family_key(name), &usable).map(|i| usable[i].key.as_str())
        };
        assert_eq!(stand_in("Helvetica-Bold"), Some("arial"));
        assert_eq!(stand_in("Times-Roman"), None);
        assert_eq!(stand_in("Garamond"), None);
    }

    #[test]
    fn an_installed_family_is_its_own_nearest() {
        let Some(dir) = std::env::var_os("CELSUS_TEST_FONTS_DIR")
            .map(PathBuf::from)
            .into_iter()
            .chain([
                PathBuf::from(r"C:\Windows\Fonts"),
                PathBuf::from("/System/Library/Fonts"),
                PathBuf::from("/usr/share/fonts"),
            ])
            .find(|p| p.is_dir())
        else {
            return;
        };
        let found = catalogue(&dir);
        let usable: Vec<&InstalledFamily> = found.iter().collect();
        for (at, family) in usable.iter().enumerate() {
            let Some(widths) = family.widths(0) else {
                continue;
            };
            let mut seen = Seen {
                class: Some(family.class),
                ..Default::default()
            };
            seen.widths[0] = *widths;
            seen.letters[0] = widths.iter().filter(|w| **w > 0.0).count();
            let near = nearest(&seen, &usable, &[]).expect("a family to compare with");
            // Two families may share their widths to the last unit, as Arial and Liberation Sans do.
            assert_eq!(seen.distance(usable[near]), Some(0.0), "{}", family.key);
            assert!(near == at || usable[near].class == family.class);
            // Every family kept can set running text: a regular face, and all four unless it is fixed pitch.
            assert!(family.faces[0].is_some());
            assert!(family.class == Family::Mono || family.faces.iter().all(Option::is_some));
        }
    }
}
