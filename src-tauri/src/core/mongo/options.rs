//! The options document of one mongosh method. Every reader names its key in the error, and the
//! key list a method accepts is exactly the list its readers read, so validation and reading
//! cannot drift.

use std::time::Duration;

use mongodb::action::Aggregate;
use mongodb::bson::{Bson, Document};
use mongodb::options::{Collation, Hint, ReturnDocument};

use crate::core::error::AppError;

/// A whole number of any BSON numeric type within `min..=max`. A Double must be finite and
/// integral, and a number beyond 2^53 is never trusted because JavaScript could not hold it.
pub fn whole_number(v: &Bson, min: i64, max: i64) -> Option<i64> {
    let n = match v {
        Bson::Int32(i) => i64::from(*i),
        Bson::Int64(i) => *i,
        Bson::Double(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 => *f as i64,
        _ => return None,
    };
    (min..=max).contains(&n).then_some(n)
}

/// An index hint: an index name or a key document. `key` names the option in the error.
pub fn hint(key: &str, value: &Bson) -> Result<Hint, AppError> {
    match value {
        Bson::String(name) => Ok(Hint::Name(name.clone())),
        Bson::Document(keys) => Ok(Hint::Keys(keys.clone())),
        _ => Err(AppError::invalid(format!(
            "{key} must be an index name or a key document"
        ))),
    }
}

/// A collation document as the driver models it. `key` names the option in the error.
pub fn collation(key: &str, value: &Document) -> Result<Collation, AppError> {
    mongodb::bson::from_document(value.clone())
        .map_err(|e| AppError::invalid(format!("Invalid {key}: {e}")))
}

/// The option keys of an `aggregate`: the read path and a writing aggregate accept the same
/// document, so there is one list.
pub const AGGREGATE_OPTIONS: &[&str] = &[
    "allowDiskUse",
    "collation",
    "hint",
    "let",
    "batchSize",
    "maxTimeMS",
];

pub const COUNT_DOCUMENTS_OPTIONS: &[&str] = &["limit", "skip", "hint", "collation", "maxTimeMS"];

pub fn unknown_option(method: &str, key: &str) -> AppError {
    AppError::invalid(format!("Unknown {method} option: {key}"))
}

/// `null` counts as absent everywhere.
pub struct Options<'a> {
    method: &'a str,
    doc: Option<&'a Document>,
}

impl<'a> Options<'a> {
    pub fn new(method: &'a str, doc: Option<&'a Document>) -> Self {
        Self { method, doc }
    }

    pub fn check(&self, known: &[&str]) -> Result<(), AppError> {
        let Some(doc) = self.doc else {
            return Ok(());
        };
        for key in doc.keys() {
            if !known.contains(&key.as_str()) {
                return Err(unknown_option(self.method, key));
            }
        }
        Ok(())
    }

    pub fn bool(&self, key: &str) -> Result<Option<bool>, AppError> {
        match self.raw(key) {
            Some(Bson::Boolean(value)) => Ok(Some(*value)),
            Some(_) => Err(AppError::invalid(format!("{key} must be true or false"))),
            None => Ok(None),
        }
    }

    pub fn string(&self, key: &str) -> Result<Option<&'a str>, AppError> {
        match self.raw(key) {
            Some(Bson::String(value)) => Ok(Some(value.as_str())),
            Some(_) => Err(AppError::invalid(format!("{key} must be a string"))),
            None => Ok(None),
        }
    }

    pub fn document(&self, key: &str) -> Result<Option<&'a Document>, AppError> {
        match self.raw(key) {
            Some(Bson::Document(value)) => Ok(Some(value)),
            Some(_) => Err(AppError::invalid(format!("{key} must be a document"))),
            None => Ok(None),
        }
    }

    pub fn whole(&self, key: &str, max: i64) -> Result<Option<i64>, AppError> {
        match self.raw(key) {
            Some(value) => whole_number(value, 0, max).map(Some).ok_or_else(|| {
                let bound = if max == i64::MAX {
                    "of 0 or more".to_string()
                } else {
                    format!("from 0 to {max}")
                };
                AppError::invalid(format!("{key} must be a whole number {bound}"))
            }),
            None => Ok(None),
        }
    }

    /// Reads any number option, as `min` and `max` of an index are.
    pub fn number(&self, key: &str) -> Result<Option<f64>, AppError> {
        match self.raw(key) {
            Some(Bson::Int32(value)) => Ok(Some(f64::from(*value))),
            Some(Bson::Int64(value)) => Ok(Some(*value as f64)),
            Some(Bson::Double(value)) => Ok(Some(*value)),
            Some(_) => Err(AppError::invalid(format!("{key} must be a number"))),
            None => Ok(None),
        }
    }

    /// Reads a number of seconds as a duration, for `expireAfterSeconds`.
    pub fn seconds(&self, key: &str) -> Result<Option<Duration>, AppError> {
        let seconds = match self.raw(key) {
            None => return Ok(None),
            Some(Bson::Int32(value)) if *value >= 0 => f64::from(*value),
            Some(Bson::Int64(value)) if *value >= 0 => *value as f64,
            Some(Bson::Double(value)) if *value >= 0.0 => *value,
            Some(_) => {
                return Err(AppError::invalid(format!(
                    "{key} must be a number of seconds"
                )));
            }
        };
        Ok(Some(Duration::from_secs(seconds as u64)))
    }

    /// Reads a version option the driver models as an enum, as `textIndexVersion` is.
    pub fn version<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, AppError> {
        match self.raw(key) {
            Some(value @ (Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_))) => {
                mongodb::bson::from_bson::<T>(value.clone())
                    .map(Some)
                    .map_err(|e| AppError::invalid(format!("Invalid {key}: {e}")))
            }
            Some(_) => Err(AppError::invalid(format!("{key} must be a whole number"))),
            None => Ok(None),
        }
    }

    pub fn hint(&self, key: &str) -> Result<Option<Hint>, AppError> {
        self.raw(key).map(|value| hint(key, value)).transpose()
    }

    pub fn collation(&self, key: &str) -> Result<Option<Collation>, AppError> {
        match self.raw(key) {
            Some(Bson::Document(value)) => Ok(Some(collation(key, value)?)),
            Some(_) => Err(AppError::invalid(format!("{key} must be a document"))),
            None => Ok(None),
        }
    }

    pub fn return_document(&self, key: &str) -> Result<Option<ReturnDocument>, AppError> {
        match self.raw(key) {
            Some(Bson::String(value)) if value == "after" => Ok(Some(ReturnDocument::After)),
            Some(Bson::String(value)) if value == "before" => Ok(Some(ReturnDocument::Before)),
            Some(_) => Err(AppError::invalid(format!(
                "{key} must be 'before' or 'after'"
            ))),
            None => Ok(None),
        }
    }

    pub fn array_filters(&self, key: &str) -> Result<Option<Vec<Document>>, AppError> {
        let wrong = || AppError::invalid(format!("{key} must be an array of documents"));
        match self.raw(key) {
            Some(Bson::Array(filters)) => filters
                .iter()
                .map(|filter| filter.as_document().cloned().ok_or_else(wrong))
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(wrong()),
            None => Ok(None),
        }
    }

    fn raw(&self, key: &str) -> Option<&'a Bson> {
        match self.doc?.get(key) {
            None | Some(Bson::Null) => None,
            Some(value) => Some(value),
        }
    }
}

/// The driver action of an `aggregate` with its options document applied. The read path and a
/// writing aggregate accept the same keys, so they build the action the same way.
pub fn apply_aggregate_options<'a, S, T>(
    mut action: Aggregate<'a, S, T>,
    o: &Options<'_>,
) -> Result<Aggregate<'a, S, T>, AppError> {
    o.check(AGGREGATE_OPTIONS)?;
    if let Some(v) = o.bool("allowDiskUse")? {
        action = action.allow_disk_use(v);
    }
    if let Some(v) = o.collation("collation")? {
        action = action.collation(v);
    }
    if let Some(v) = o.hint("hint")? {
        action = action.hint(v);
    }
    if let Some(v) = o.document("let")? {
        action = action.let_vars(v.clone());
    }
    if let Some(n) = o.whole("batchSize", i64::from(u32::MAX))? {
        action = action.batch_size(n as u32);
    }
    if let Some(ms) = o.whole("maxTimeMS", i64::MAX)? {
        action = action.max_time(Duration::from_millis(ms as u64));
    }
    Ok(action)
}

#[cfg(test)]
mod tests {
    use mongodb::bson::doc;

    use super::*;

    #[test]
    fn whole_numbers_keep_their_type_and_bounds() {
        assert_eq!(whole_number(&Bson::Int32(7), 0, i64::MAX), Some(7));
        assert_eq!(whole_number(&Bson::Int64(-7), i64::MIN, i64::MAX), Some(-7));
        assert_eq!(whole_number(&Bson::Double(42.0), 0, i64::MAX), Some(42));
        assert_eq!(whole_number(&Bson::Double(1.5), 0, i64::MAX), None);
        assert_eq!(whole_number(&Bson::Double(f64::NAN), 0, i64::MAX), None);
        assert_eq!(whole_number(&Bson::Double(9.0e15), 0, i64::MAX), None);
        assert_eq!(whole_number(&Bson::Int32(-1), 0, i64::MAX), None);
        assert_eq!(whole_number(&Bson::Int64(4_294_967_296), 0, 4_294_967_295), None);
        assert_eq!(whole_number(&Bson::String("1".into()), 0, i64::MAX), None);
    }

    #[test]
    fn readers_name_the_key_and_null_is_absent() {
        let options = doc! { "ordered": false, "batch": 5, "at": Bson::Null };
        let o = Options::new("insertMany", Some(&options));
        assert_eq!(o.bool("ordered").unwrap(), Some(false));
        assert_eq!(o.bool("at").unwrap(), None);
        assert_eq!(o.whole("batch", i64::MAX).unwrap(), Some(5));
        assert_eq!(
            o.bool("batch").unwrap_err().message,
            "batch must be true or false"
        );
        assert_eq!(
            o.whole("ordered", i64::MAX).unwrap_err().message,
            "ordered must be a whole number of 0 or more"
        );
        assert_eq!(
            o.whole("batch", 4).unwrap_err().message,
            "batch must be a whole number from 0 to 4"
        );
    }

    #[test]
    fn unknown_keys_are_refused_in_document_order() {
        let options = doc! { "upsert": true, "nope": 1, "bogus": 2 };
        let o = Options::new("updateOne", Some(&options));
        assert_eq!(
            o.check(&["upsert"]).unwrap_err(),
            unknown_option("updateOne", "nope")
        );
        assert_eq!(
            unknown_option("aggregate", "x").message,
            "Unknown aggregate option: x"
        );
    }
}
