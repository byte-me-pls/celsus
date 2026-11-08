//! The PDF object model.

use std::collections::BTreeMap;
use std::fmt;

/// An indirect object reference, `num generation R`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjRef {
    pub num: u32,
    pub generation: u16,
}

impl fmt::Display for ObjRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.num, self.generation)
    }
}

/// Dictionary keys are names without the leading slash.
pub type Dict = BTreeMap<Vec<u8>, Object>;

#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    /// The data as stored in the file, still encoded with the stream's filters.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    String(Vec<u8>),
    Name(Vec<u8>),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Stream),
    Ref(ObjRef),
}

impl Object {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Object::Int(i) => Some(*i),
            Object::Real(r) => Some(*r as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Object::Int(i) => Some(*i as f64),
            Object::Real(r) => Some(*r),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_str_bytes(&self) -> Option<&[u8]> {
        match self {
            Object::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The dictionary of a dictionary or of a stream.
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn as_stream(&self) -> Option<&Stream> {
        match self {
            Object::Stream(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_ref(&self) -> Option<ObjRef> {
        match self {
            Object::Ref(r) => Some(*r),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Object::Null)
    }
}

/// Read a numeric array such as a rectangle or matrix.
pub fn numbers(obj: &Object) -> Option<Vec<f64>> {
    obj.as_array()?.iter().map(Object::as_f64).collect()
}

/// Look up `key` in a dictionary.
pub fn get<'d>(dict: &'d Dict, key: &str) -> Option<&'d Object> {
    dict.get(key.as_bytes())
}
