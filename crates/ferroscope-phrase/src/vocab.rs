//! The words this understands, in one place so the error messages can list them.
//!
//! Kept as data rather than scattered through `match` arms for one reason: when the parser does
//! not understand a word, it has to be able to say what it *would* have understood. A vocabulary
//! you cannot enumerate is a vocabulary you cannot apologise for.

/// A shape word and the schema shape it means.
pub const SHAPES: &[(&str, &str)] = &[
    ("box", "box"),
    ("boxes", "box"),
    ("crate", "box"),
    ("crates", "box"),
    ("block", "box"),
    ("blocks", "box"),
    ("cube", "box"),
    ("cubes", "box"),
    ("pallet", "box"),
    ("brick", "box"),
    ("sphere", "sphere"),
    ("spheres", "sphere"),
    ("ball", "sphere"),
    ("balls", "sphere"),
    ("marble", "sphere"),
    ("beacon", "sphere"),
    ("cylinder", "cylinder"),
    ("cylinders", "cylinder"),
    ("tube", "cylinder"),
    ("can", "cylinder"),
    ("cans", "cylinder"),
    ("pillar", "cylinder"),
    ("post", "cylinder"),
    ("drum", "cylinder"),
    ("barrel", "cylinder"),
];

/// A motion word and the schema motion it means.
pub const MOTIONS: &[(&str, &str)] = &[
    ("drop", "fall"),
    ("drops", "fall"),
    ("dropped", "fall"),
    ("dropping", "fall"),
    ("fall", "fall"),
    ("falls", "fall"),
    ("falling", "fall"),
    ("bounce", "fall"),
    ("bounces", "fall"),
    ("bouncing", "fall"),
    ("orbit", "orbit"),
    ("orbits", "orbit"),
    ("orbiting", "orbit"),
    ("circle", "orbit"),
    ("circles", "orbit"),
    ("circling", "orbit"),
    ("spin", "orbit"),
    ("spinning", "orbit"),
    ("slide", "linear"),
    ("slides", "linear"),
    ("sliding", "linear"),
    ("travel", "linear"),
    ("travels", "linear"),
    ("travelling", "linear"),
    ("traveling", "linear"),
    ("shuttle", "linear"),
    ("shuttles", "linear"),
    ("shuttling", "linear"),
    ("oscillate", "oscillate"),
    ("oscillates", "oscillate"),
    ("oscillating", "oscillate"),
    ("vibrate", "oscillate"),
    ("vibrates", "oscillate"),
    ("vibrating", "oscillate"),
    ("shake", "oscillate"),
    ("shakes", "oscillate"),
    ("shaking", "oscillate"),
    ("sway", "oscillate"),
    ("sit", "static"),
    ("sits", "static"),
    ("sitting", "static"),
    ("rest", "static"),
    ("rests", "static"),
    ("resting", "static"),
    ("still", "static"),
    ("static", "static"),
    ("stationary", "static"),
];

/// Robot words and the built-in description they name.
pub const ROBOTS: &[(&str, &str)] = &[
    ("so101", "so101"),
    ("so-101", "so101"),
    ("lerobot", "so101"),
    ("arm", "arm"),
    ("manipulator", "arm"),
    ("robot", "so101"),
];

/// Colour words. Named rather than computed so an unknown colour can be refused by name.
pub const COLOURS: &[(&str, &str)] = &[
    ("red", "#d4483b"),
    ("orange", "#e08a3c"),
    ("yellow", "#e0c23c"),
    ("green", "#4caf6d"),
    ("teal", "#46c6b0"),
    ("blue", "#4a7fd4"),
    ("violet", "#9d8cff"),
    ("purple", "#9d8cff"),
    ("pink", "#e07ba8"),
    ("brown", "#a0703c"),
    ("tan", "#c9a06a"),
    ("grey", "#8a93a6"),
    ("gray", "#8a93a6"),
    ("white", "#e8ecf4"),
    ("black", "#1b2036"),
    ("gold", "#cfaa5b"),
];

/// Material words and the table id they resolve to.
///
/// Deliberately a short list of the ones people say out loud, not the whole 437-row table: this
/// maps *English* to an id, and `materials_search` is there for the rest.
pub const MATERIALS: &[(&str, &str)] = &[
    ("aluminium", "6061-T6"),
    ("aluminum", "6061-T6"),
    ("steel", "1018"),
    ("titanium", "Ti-6Al-4V"),
    ("pla", "PLA"),
    ("abs", "ABS"),
    ("nylon", "Nylon 6"),
    ("brass", "C36000"),
    ("copper", "C11000"),
];

/// Words with a fixed numeric meaning.
pub const NUMBER_WORDS: &[(&str, f64)] = &[
    ("a", 1.0),
    ("an", 1.0),
    ("one", 1.0),
    ("two", 2.0),
    ("three", 3.0),
    ("four", 4.0),
    ("five", 5.0),
    ("six", 6.0),
    ("seven", 7.0),
    ("eight", 8.0),
    ("nine", 9.0),
    ("ten", 10.0),
];

/// Words that carry no scene meaning and are not worth reporting as ignored.
///
/// Without this every sentence would come back with a paragraph of "I did not understand `the`",
/// which buries the one word that actually mattered.
pub const FILLER: &[&str] = &[
    "the",
    "a",
    "an",
    "and",
    "then",
    "with",
    "of",
    "on",
    "in",
    "to",
    "at",
    "is",
    "are",
    "it",
    "its",
    "that",
    "this",
    "there",
    "here",
    "some",
    "please",
    "show",
    "me",
    "make",
    "create",
    "build",
    "add",
    "put",
    "have",
    "has",
    "let",
    "lets",
    "want",
    "would",
    "like",
    "see",
    "watch",
    "scene",
    "simulation",
    "sim",
    "run",
    "record",
    "next",
    "beside",
    "near",
    "onto",
    "into",
    "from",
    "for",
    "over",
    "each",
    "every",
    "high",
    "up",
    "down",
    "above",
    "below",
    "while",
    "as",
    "by",
    "be",
    "do",
    "does",
    "just",
    "very",
    "also",
    "plus",
    "along",
    "around",
    "off",
    // Direction words that only ever restate a motion already named by its verb.
    "back",
    "forth",
    "forwards",
    "backwards",
    "again",
    "repeatedly",
    "continuously",
    "slowly",
    "quickly",
    "gently",
    "radius",
    "apart",
    "together",
    "onto",
    "upon",
    "towards",
    "toward",
];

pub fn lookup<'a>(table: &[(&'a str, &'a str)], word: &str) -> Option<&'a str> {
    table.iter().find(|(k, _)| *k == word).map(|(_, v)| *v)
}

/// A comma-joined list of the distinct meanings in a table, for an error message.
pub fn meanings(table: &[(&str, &str)]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for (_, v) in table {
        if !seen.contains(v) {
            seen.push(v);
        }
    }
    seen.join(", ")
}
