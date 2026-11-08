//! A small PDF library written for Celsus.
//!
//! It covers what translating a book needs and nothing more: reading any PDF a book is likely to come in, finding
//! every piece of text with its exact position, font, size and colour, and writing a translated page back by
//! changing only the text that was translated. Everything else on a page is left byte for byte as it was.

pub mod cmap;
pub mod content;
pub mod document;
pub mod edit;
pub mod embed;
pub mod encoding;
pub mod filters;
pub mod font;
pub mod interp;
pub mod lexer;
pub mod object;
pub mod ttf;
pub mod write;

pub use document::{Document, Page};
pub use object::{Dict, ObjRef, Object, Stream};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Syntax(&'static str, usize),
    Corrupt(String),
    Unsupported(String),
    Encrypted,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Syntax(what, at) => write!(f, "syntax error: {what} at byte {at}"),
            Error::Corrupt(what) => write!(f, "damaged file: {what}"),
            Error::Unsupported(what) => write!(f, "unsupported: {what}"),
            Error::Encrypted => write!(f, "the file is encrypted"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
