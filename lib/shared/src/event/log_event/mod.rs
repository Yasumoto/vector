#![allow(clippy::needless_collect)]

pub mod lua;

use crate::{event::*, lookup::*};
use serde::{Deserialize, Serialize};
use std::{
    collections::{btree_map::Entry, BTreeMap, HashMap},
    convert::{TryFrom, TryInto},
    fmt::Debug,
    iter::FromIterator,
};
use tracing::{instrument, trace_span, trace, error};

/// A map of [`crate::event::Value`].
///
/// The inside of an [`Event::Log`](crate::event::Event) variant of [`crate::event::Event`].
///
/// This type supports being interacted with like a regular old
/// [`BTreeMap`](std::collections::BTreeMap), or with special (unowned) [`crate::event::Lookup`] and
/// (owned) [`crate::event::LookupBuf`] types.
///
/// Transparently, as a normal [`BTreeMap`](std::collections::BTreeMap):
///
/// ```rust
/// use shared::{event::*, lookup::*};
/// let mut event = LogEvent::default();
/// event.insert(String::from("foo"), 1);
/// assert!(event.contains("foo"));
/// assert_eq!(event.get("foo"), Some(&Value::from(1)));
/// ```
///
/// Using remap-style lookups:
///
/// ```rust
/// use shared::{event::*, lookup::*};
/// let lookup = LookupBuf::from_str("foo[0].(bar | bat)").unwrap();
/// event.insert(lookup.clone(), 1);
/// assert!(event.contains(&lookup));
/// assert_eq!(event.get(&lookup), Some(&Value::from(1)));
/// ```
///
/// It's possible to access the inner [`BTreeMap`](std::collections::BTreeMap):
///
/// ```rust
/// use shared::{event::*, lookup::*};
/// let mut event = LogEvent::default();
/// event.insert(String::from("foo"), 1);
///
/// use std::collections::BTreeMap;
/// let _inner: &BTreeMap<_, _> = event.inner();
/// let _inner: &mut BTreeMap<_, _> = event.inner_mut();
/// let inner: BTreeMap<_, _> = event.take();
///
/// let event = LogEvent::from(inner);
/// ```
///
/// There exists a `log_event` macro you may also utilize to create this type:
///
/// ```rust
/// use shared::{event::*, lookup::*};
/// let event = log_event! {
///     "foo" => 1,
///     LookupBuf::from_str("bar.baz").unwrap() => 2,
/// }.into_log();
/// assert!(event.contains("foo"));
/// assert!(event.contains(Lookup::from_str("foo").unwrap()));
/// ```
#[derive(PartialEq, Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogEvent {
    #[serde(flatten)]
    fields: BTreeMap<String, Value>,
}

impl LogEvent {
    /// Get an immutable borrow of the given value by lookup.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let plain_key = "foo";
    /// let lookup_key = LookupBuf::from_str("bar.baz").unwrap();
    /// let event = log_event! {
    ///     plain_key => 1,
    ///     lookup_key.clone() => 2,
    /// }.into_log();
    /// assert_eq!(event.get(plain_key), Some(&vector::event::Value::from(1)));
    /// assert_eq!(event.get(&lookup_key), Some(&vector::event::Value::from(2)));
    /// ```
    pub fn get<'a>(&self, lookup: impl Into<Lookup<'a>> + Debug) -> Option<&Value> {
        let mut working_lookup = lookup.into();
        let span = trace_span!("get", lookup = %working_lookup);
        let _guard = span.enter();

        // The first step should always be a field.
        let this_segment = working_lookup.pop_front().unwrap();
        // This is good, since the first step into a LogEvent will also be a field.

        // This step largely exists so that we can make `cursor` a `Value` right off the bat.
        // We couldn't go like `let cursor = Value::from(self.fields)` since that'd take the value.
        match this_segment {
            Segment::Coalesce(sub_segments) => {
                // Creating a needle with a back out of the loop is very important.
                let mut needle = None;
                for sub_segment in sub_segments {
                    let mut lookup = Lookup::try_from(sub_segment).ok()?;
                    // Notice we cannot take multiple mutable borrows in a loop, so we must pay the
                    // contains cost extra. It's super unfortunate, hopefully future work can solve this.
                    lookup.extend(working_lookup.clone()); // We need to include the rest of the removal.
                    if self.contains(lookup.clone()) {
                        trace!(option = %lookup, "Found coalesce option.");
                        needle = Some(lookup);
                        break;
                    } else {
                        trace!(option = %lookup, "Did not find coalesce option.");
                    }
                }
                match needle {
                    Some(needle) => self.get(needle),
                    None => None,
                }
            }
            Segment::Field {
                name,
                requires_quoting: _,
            } => {
                if working_lookup.len() == 0 {
                    // Terminus: We **must** insert here or abort.
                    trace!(field = %name, "Getting from root.");
                    self.fields.get(name)
                } else {
                    trace!(field = %name, "Descending into map.");
                    match self.fields.get(name) {
                        Some(v) => v.get(working_lookup).unwrap_or_else(|e| {
                            trace!(?e);
                            None
                        }),
                        None => None,
                    }
                }
            }
            // In this case, the user has passed us an invariant.
            Segment::Index(_) => {
                error!(
                    "Lookups into LogEvents should never start with indexes.\
                        Please report your config."
                );
                None
            }
        }
    }

    /// Get a mutable borrow of the value by lookup.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let plain_key = "foo";
    /// let lookup_key = LookupBuf::from_str("bar.baz").unwrap();
    /// let mut event = log_event! {
    ///     plain_key => 1,
    ///     lookup_key.clone() => 2,
    /// }.into_log();
    /// assert_eq!(event.get_mut(plain_key), Some(&mut vector::event::Value::from(1)));
    /// assert_eq!(event.get_mut(&lookup_key), Some(&mut vector::event::Value::from(2)));
    /// ```
    pub fn get_mut<'a>(&mut self, lookup: impl Into<Lookup<'a>> + Debug) -> Option<&mut Value> {
        let mut working_lookup = lookup.into();
        let span = trace_span!("get_mut", lookup = %working_lookup);
        let _guard = span.enter();

        // The first step should always be a field.
        let this_segment = working_lookup.pop_front().unwrap();
        // This is good, since the first step into a LogEvent will also be a field.

        // This step largely exists so that we can make `cursor` a `Value` right off the bat.
        // We couldn't go like `let cursor = Value::from(self.fields)` since that'd take the value.
        match this_segment {
            Segment::Coalesce(sub_segments) => {
                // Creating a needle with a back out of the loop is very important.
                let mut needle = None;
                for sub_segment in sub_segments {
                    let mut lookup = Lookup::try_from(sub_segment).ok()?;
                    // Notice we cannot take multiple mutable borrows in a loop, so we must pay the
                    // contains cost extra. It's super unfortunate, hopefully future work can solve this.
                    lookup.extend(working_lookup.clone()); // We need to include the rest of the removal.
                    if self.contains(lookup.clone()) {
                        trace!(option = %lookup, "Found coalesce option.");
                        needle = Some(lookup);
                        break;
                    } else {
                        trace!(option = %lookup, "Did not find coalesce option.");
                    }
                }
                match needle {
                    Some(needle) => self.get_mut(needle),
                    None => None,
                }
            }
            Segment::Field {
                name,
                requires_quoting: _,
            } => {
                if working_lookup.len() == 0 {
                    // Terminus: We **must** insert here or abort.
                    trace!(field = %name, "Getting from root.");
                    self.fields.get_mut(name)
                } else {
                    trace!(field = %name, "Descending into map.");
                    match self.fields.get_mut(name) {
                        Some(v) => v.get_mut(working_lookup).unwrap_or_else(|e| {
                            trace!(?e);
                            None
                        }),
                        None => None,
                    }
                }
            }
            // In this case, the user has passed us an invariant.
            Segment::Index(_) => {
                error!(
                    "Lookups into LogEvents should never start with indexes.\
                        Please report your config."
                );
                None
            }
        }
    }

    /// Determine if the log event contains a value at a given lookup.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let plain_key = "foo";
    /// let lookup_key = LookupBuf::from_str("bar.baz").unwrap();
    /// let mut event = log_event! {
    ///     plain_key => 1,
    ///     lookup_key.clone() => 2,
    /// }.into_log();
    /// assert!(event.contains(plain_key));
    /// assert!(event.contains(&lookup_key));
    /// ```
    pub fn contains<'a>(&self, lookup: impl Into<Lookup<'a>> + Debug) -> bool {
        let working_lookup = lookup.into();
        let span = trace_span!("contains", lookup = %working_lookup);
        let _guard = span.enter();

        self.get(working_lookup).is_some()
    }

    /// Insert a value at a given lookup, returning any old value that exists.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let plain_key = "foo";
    /// let lookup_key = LookupBuf::from_str("bar.baz").unwrap();
    /// let mut event = log_event! {
    ///     plain_key => 1,
    ///     lookup_key.clone() => 2,
    /// }.into_log();
    /// assert_eq!(event.insert(plain_key, i64::MAX), Some(vector::event::Value::from(1)));
    /// assert_eq!(event.insert(lookup_key.clone(), i64::MAX), Some(vector::event::Value::from(2)));
    /// ```
    pub fn insert(
        &mut self,
        lookup: impl Into<LookupBuf>,
        value: impl Into<Value> + Debug,
    ) -> Option<Value> {
        let mut working_lookup: LookupBuf = lookup.into();
        let span = trace_span!("insert", lookup = %working_lookup);
        let _guard = span.enter();

        // The first step should always be a field.
        let this_segment = working_lookup.pop_front().unwrap();
        // This is good, since the first step into a LogEvent will also be a field.

        // This step largely exists so that we can make `cursor` a `Value` right off the bat.
        // We couldn't go like `let cursor = Value::from(self.fields)` since that'd take the value.
        match this_segment {
            SegmentBuf::Coalesce(sub_segments) => {
                trace!("Seeking first match of coalesce.");
                // Creating a needle with a back out of the loop is very important.
                let mut needle = None;
                for sub_segment in sub_segments {
                    let mut lookup = LookupBuf::try_from(sub_segment).ok()?;
                    // Notice we cannot take multiple mutable borrows in a loop, so we must pay the
                    // contains cost extra. It's super unfortunate, hopefully future work can solve this.
                    lookup.extend(working_lookup.clone()); // We need to include the rest of the removal.
                    if !self.contains(&lookup) {
                        trace!(option = %lookup, "Found coalesce option.");
                        needle = Some(lookup);
                        break;
                    } else {
                        trace!(option = %lookup, "Did not find coalesce option.");
                    }
                }
                match needle {
                    Some(needle) => self.insert(needle, value),
                    None => None,
                }
            }
            SegmentBuf::Field {
                name,
                requires_quoting: _,
            } => {
                let next_value = match working_lookup.get(0) {
                    Some(SegmentBuf::Index(_)) => Value::Array(Vec::with_capacity(0)),
                    Some(SegmentBuf::Field { .. }) => Value::Map(Default::default()),
                    Some(SegmentBuf::Coalesce(set)) => {
                        let mut cursor_set = set;
                        loop {
                            match cursor_set.get(0).and_then(|v| v.get(0)) {
                                None => return None,
                                Some(SegmentBuf::Field { .. }) => {
                                    break Value::Map(Default::default())
                                }
                                Some(SegmentBuf::Index(i)) => {
                                    break Value::Array(Vec::with_capacity(*i))
                                }
                                Some(SegmentBuf::Coalesce(set)) => cursor_set = &set,
                            }
                        }
                    }
                    None => {
                        trace!(field = %name, "Getting from root.");
                        return self.fields.insert(name, value.into());
                    }
                };
                trace!(field = %name, "Seeking into map.");
                self.fields
                    .entry(name)
                    .or_insert_with(|| {
                        trace!("Inserting at leaf.");
                        next_value
                    })
                    .insert(working_lookup, value)
                    .unwrap_or_else(|e| {
                        trace!(?e);
                        None
                    })
            }
            // In this case, the user has passed us an invariant.
            SegmentBuf::Index(_) => {
                error!(
                    "Lookups into LogEvents should never start with indexes.\
                        Please report your config."
                );
                None
            }
        }
    }

    /// Remove a value that exists at a given lookup.
    ///
    /// Setting `prune` to true will also remove the entries of maps and arrays that are emptied.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let plain_key = "foo";
    /// let lookup_key = LookupBuf::from_str("bar.baz.slam").unwrap();
    /// let mut event = log_event! {
    ///     plain_key => 1,
    ///     lookup_key.clone() => 2,
    /// }.into_log();
    /// assert_eq!(event.remove(plain_key, true), Some(vector::event::Value::from(1)));
    /// assert_eq!(event.remove(&lookup_key, true), Some(vector::event::Value::from(2)));
    /// // Since we pruned, observe how `bar` is also removed because `prune` is set:
    /// assert!(!event.contains("bar.baz"));
    /// ```
    pub fn remove<'lookup>(
        &mut self,
        lookup: impl Into<Lookup<'lookup>> + Debug,
        prune: bool,
    ) -> Option<Value> {
        let mut working_lookup = lookup.into();
        let span = trace_span!("remove", lookup = %working_lookup);
        let _guard = span.enter();

        // The first step should always be a field.
        let this_segment = working_lookup.pop_front().unwrap();
        // This step largely exists so that we can make `cursor` a `Value` right off the bat.
        // We couldn't go like `let cursor = Value::from(self.fields)` since that'd take the value.
        match this_segment {
            Segment::Coalesce(sub_segments) => {
                trace!("Seeking first match of coalesce.");
                // Creating a needle with a back out of the loop is very important.
                let mut needle = None;
                for sub_segment in sub_segments {
                    let mut lookup = Lookup::try_from(sub_segment).ok()?;
                    // Notice we cannot take multiple mutable borrows in a loop, so we must pay the
                    // contains cost extra. It's super unfortunate, hopefully future work can solve this.
                    lookup.extend(working_lookup.clone()); // We need to include the rest of the removal.
                    if self.contains(lookup.clone()) {
                        trace!(option = %lookup, "Found coalesce option.");
                        needle = Some(lookup);
                        break;
                    } else {
                        trace!(option = %lookup, "Did not find coalesce option.");
                    }
                }
                match needle {
                    Some(needle) => self.remove(needle, prune),
                    None => None,
                }
            }
            Segment::Field {
                name,
                requires_quoting: _,
            } => {
                if working_lookup.len() == 0 {
                    // Terminus: We **must** insert here or abort.
                    trace!(field = %name, "Getting from root.");
                    let retval = self.fields.remove(name);
                    if prune && self.fields.get(name) == Some(&Value::Null) {
                        self.fields.remove(name);
                    }
                    retval
                } else {
                    trace!(field = %name, "Seeking into map.");
                    let retval = match self.fields.get_mut(name) {
                        Some(v) => v.remove(working_lookup, prune).unwrap_or_else(|e| {
                            trace!(?e);
                            None
                        }),
                        None => None,
                    };
                    if prune && self.fields.get(name) == Some(&Value::Null) {
                        self.fields.remove(name);
                    }
                    retval
                }
            }
            // In this case, the user has passed us an invariant.
            Segment::Index(_) => {
                error!(
                    "Lookups into LogEvents should never start with indexes.\
                        Please report your config."
                );
                None
            }
        }
    }

    /// Iterate over the lookups available in this log event.
    ///
    /// This is notably different than the keys in a map, as this descends into things like arrays
    /// and maps. It also returns those array/map values during iteration.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let plain_key = "lick";
    /// let lookup_key = LookupBuf::from_str("vic.stick.slam").unwrap();
    /// let event = log_event! {
    ///     plain_key => 1,
    ///     lookup_key.clone() => 2,
    /// }.into_log();
    /// let mut keys = event.keys(false);
    /// assert_eq!(keys.next(), Some(Lookup::from_str("lick").unwrap()));
    /// assert_eq!(keys.next(), Some(Lookup::from_str("vic").unwrap()));
    /// assert_eq!(keys.next(), Some(Lookup::from_str("vic.stick").unwrap()));
    /// assert_eq!(keys.next(), Some(Lookup::from_str("vic.stick.slam").unwrap()));
    ///
    /// let mut keys = event.keys(true);
    /// assert_eq!(keys.next(), Some(Lookup::from_str("lick").unwrap()));
    /// assert_eq!(keys.next(), Some(Lookup::from_str("vic.stick.slam").unwrap()));
    /// ```
    #[instrument(level = "trace", skip(self, only_leaves))]
    pub fn keys<'a>(&'a self, only_leaves: bool) -> impl Iterator<Item = Lookup<'a>> + 'a {
        self.fields
            .iter()
            .map(move |(k, v)| {
                let lookup = Lookup::from(k);
                v.lookups(Some(lookup), only_leaves)
            })
            .flatten()
    }

    /// Iterate over all lookup/value pairs.
    ///
    /// This is notably different than pairs in a map, as this descends into things like arrays and
    /// maps. It also returns those array/map values during iteration.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let plain_key = "lick";
    /// let lookup_key = LookupBuf::from_str("vic.stick.slam").unwrap();
    /// let event = log_event! {
    ///     plain_key => 1,
    ///     lookup_key.clone() => 2,
    /// }.into_log();
    /// let mut keys = event.pairs(false);
    /// assert_eq!(keys.next(), Some((Lookup::from_str("lick").unwrap(), &Value::from(1))));
    /// assert_eq!(keys.next(), Some((Lookup::from_str("vic").unwrap(), &Value::from({
    ///     let mut inner_map = std::collections::BTreeMap::default();
    ///     inner_map.insert(String::from("slam"), Value::from(2));
    ///     let mut map = std::collections::BTreeMap::default();
    ///     map.insert(String::from("stick"), Value::from(inner_map));
    ///     map
    /// }))));
    /// assert_eq!(keys.next(), Some((Lookup::from_str("vic.stick").unwrap(), &Value::from({
    ///     let mut map = std::collections::BTreeMap::default();
    ///     map.insert(String::from("slam"), Value::from(2));
    ///     map
    /// }))));
    /// assert_eq!(keys.next(), Some((Lookup::from_str("vic.stick.slam").unwrap(), &Value::from(2))));
    ///
    /// let mut keys = event.pairs(true);
    /// assert_eq!(keys.next(), Some((Lookup::from_str("lick").unwrap(), &Value::from(1))));
    /// assert_eq!(keys.next(), Some((Lookup::from_str("vic.stick.slam").unwrap(), &Value::from(2))));
    /// ```
    #[instrument(level = "trace", skip(self, only_leaves))]
    pub fn pairs<'a>(&'a self, only_leaves: bool) -> impl Iterator<Item = (Lookup<'a>, &'a Value)> {
        self.fields
            .iter()
            .map(move |(k, v)| {
                let lookup = Lookup::from(k);
                v.pairs(Some(lookup), only_leaves)
            })
            .flatten()
    }

    /// Determine if the log event is empty of fields.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let event = LogEvent::default();
    /// assert!(event.is_empty());
    /// ```
    #[instrument(level = "trace", skip(self))]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Return an entry for the given lookup.
    #[instrument(level = "trace", skip(self, lookup), fields(lookup = %lookup), err)]
    pub fn entry(&mut self, lookup: LookupBuf) -> crate::Result<Entry<String, Value>> {
        trace!("Seeking to entry.");
        let mut walker = lookup.into_iter().enumerate();

        let mut current_pointer = if let Some((
            index,
            SegmentBuf::Field {
                name: segment,
                requires_quoting: _,
            },
        )) = walker.next()
        {
            trace!(%segment, index, "Seeking segment.");
            self.fields.entry(segment)
        } else {
            unreachable!(
                "It is an invariant to have a `Lookup` without a contained `Segment`.\
                `Lookup::is_valid` should catch this during `Lookup` creation, maybe it was not \
                called?."
            );
        };

        for (index, segment) in walker {
            trace!(%segment, index, "Seeking next segment.");
            current_pointer = match (segment, current_pointer) {
                (
                    SegmentBuf::Field {
                        name,
                        requires_quoting: _,
                    },
                    Entry::Occupied(entry),
                ) => match entry.into_mut() {
                    Value::Map(map) => map.entry(name),
                    v => return Err(format!("Looking up field on a non-map value: {:?}", v).into()),
                },
                (
                    SegmentBuf::Field {
                        name,
                        requires_quoting: _,
                    },
                    Entry::Vacant(entry),
                ) => {
                    trace!(segment = %name, index, "Met vacant entry.");
                    return Err(format!(
                        "Tried to step into `{}` of `{}`, but it did not exist.",
                        name,
                        entry.key()
                    )
                    .into());
                }
                _ => return Err("The entry API cannot yet descend into array indices.".into()),
            };
        }
        trace!(entry = ?current_pointer, "Result.");
        Ok(current_pointer)
    }

    /// Returns the entire event as a `Value::Map`.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let event = LogEvent::default();
    /// assert_eq!(event.take(), std::collections::BTreeMap::default());
    /// ```
    #[instrument(level = "trace", skip(self))]
    pub fn take(self) -> BTreeMap<String, Value> {
        self.fields
    }

    /// Get a borrow of the contained fields.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let mut event = LogEvent::default();
    /// assert_eq!(event.inner(), &std::collections::BTreeMap::default());
    /// ```
    #[instrument(level = "trace", skip(self))]
    pub fn inner(&mut self) -> &BTreeMap<String, Value> {
        &self.fields
    }

    /// Get a mutable borrow of the contained fields.
    ///
    /// ```rust
    /// use shared::{event::*, lookup::*};
    /// let mut event = LogEvent::default();
    /// assert_eq!(event.inner_mut(), &mut std::collections::BTreeMap::default());
    /// ```
    #[instrument(level = "trace", skip(self))]
    pub fn inner_mut(&mut self) -> &mut BTreeMap<String, Value> {
        &mut self.fields
    }
}

impl From<BTreeMap<String, Value>> for LogEvent {
    fn from(map: BTreeMap<String, Value>) -> Self {
        LogEvent { fields: map }
    }
}

impl Into<BTreeMap<String, Value>> for LogEvent {
    fn into(self) -> BTreeMap<String, Value> {
        let Self { fields } = self;
        fields
    }
}

impl From<HashMap<String, Value>> for LogEvent {
    fn from(map: HashMap<String, Value>) -> Self {
        LogEvent {
            fields: map.into_iter().collect(),
        }
    }
}

impl Into<HashMap<String, Value>> for LogEvent {
    fn into(self) -> HashMap<String, Value> {
        self.fields.into_iter().collect()
    }
}

impl TryFrom<serde_json::Value> for LogEvent {
    type Error = crate::Error;

    fn try_from(map: serde_json::Value) -> Result<Self, Self::Error> {
        match map {
            serde_json::Value::Object(fields) => Ok(LogEvent::from(
                fields
                    .into_iter()
                    .map(|(k, v)| (k, v.into()))
                    .collect::<BTreeMap<_, _>>(),
            )),
            _ => Err(crate::Error::from(
                "Attempted to convert non-Object JSON into a LogEvent.",
            )),
        }
    }
}

impl TryInto<serde_json::Value> for LogEvent {
    type Error = crate::Error;

    fn try_into(self) -> Result<serde_json::Value, Self::Error> {
        let Self { fields } = self;
        Ok(serde_json::to_value(fields)?)
    }
}

impl<'a, V> Extend<(LookupBuf, V)> for LogEvent
where
    V: Into<Value>,
{
    fn extend<I: IntoIterator<Item = (LookupBuf, V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v.into());
        }
    }
}

// Allow converting any kind of appropriate key/value iterator directly into a LogEvent.
impl<'a, V: Into<Value>> FromIterator<(LookupBuf, V)> for LogEvent {
    fn from_iter<T: IntoIterator<Item = (LookupBuf, V)>>(iter: T) -> Self {
        let mut log_event = LogEvent::default();
        log_event.extend(iter);
        log_event
    }
}

/// Converts event into an iterator over top-level key/value pairs.
impl IntoIterator for LogEvent {
    type Item = (String, Value);
    type IntoIter = std::collections::btree_map::IntoIter<String, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.fields.into_iter()
    }
}

impl<T> std::ops::Index<T> for LogEvent
where
    T: Into<Lookup<'static>> + Debug,
{
    type Output = Value;

    fn index(&self, key: T) -> &Value {
        self.get(key).expect("Key not found.")
    }
}

impl<T> std::ops::IndexMut<T> for LogEvent
where
    T: Into<Lookup<'static>> + Debug,
{
    fn index_mut(&mut self, key: T) -> &mut Value {
        self.get_mut(key).expect("Key not found.")
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json::json;
    use tracing::trace;
    use test_env_log::test;

    mod insert_get_remove {
        use super::*;

        #[test_env_log::test]
        fn root() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }

        #[test_env_log::test]
        fn quoted_from_str() -> crate::Result<()> {
            // In this test, we make sure the quotes are stripped, since it's a parsed lookup.
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root.\"doot\"")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"].as_map()["doot"], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }

        #[test_env_log::test]
        fn root_with_buddy() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));

            let lookup = LookupBuf::from_str("scrubby")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["scrubby"], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }

        #[test_env_log::test]
        fn coalesced_root() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("(snoot | boot).loot")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["snoot"].as_map()["loot"], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));

            let lookup = LookupBuf::from_str("boot")?;
            assert_eq!(event.get(&lookup), None);

            Ok(())
        }

        #[test_env_log::test]
        fn coalesced_nested() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root.(snoot | boot)")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"].as_map()["snoot"], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));

            let lookup = LookupBuf::from_str("root.boot")?;
            assert_eq!(event.get(&lookup), None);

            Ok(())
        }

        #[test_env_log::test]
        fn coalesced_with_nesting() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root.(snoot | boot.beep).leep")?;
            let mut value = Value::Boolean(true);

            // This is deliberately duplicated!!! Because it's a coalesce both fields will be filled.
            // This is the point of the test!
            event.insert(lookup.clone(), value.clone());
            event.insert(lookup.clone(), value.clone());

            assert_eq!(
                event.inner()["root"].as_map()["snoot"].as_map()["leep"],
                value
            );
            assert_eq!(
                event.inner()["root"].as_map()["boot"].as_map()["beep"].as_map()["leep"],
                value
            );

            // This repeats, because it's the purpose of the test!
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value.clone()));
            // Now that we removed one, we will get the other.
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));

            Ok(())
        }
        #[test_env_log::test]
        fn map_field() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root.field")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"].as_map()["field"], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }

        #[test_env_log::test]
        fn nested_map_field() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root.field.subfield")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(
                event.inner()["root"].as_map()["field"].as_map()["subfield"],
                value
            );
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }

        #[test_env_log::test]
        fn array_field() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root[0]")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"].as_array()[0], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }

        #[test_env_log::test]
        fn array_reverse_population() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root[2]")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"].as_array()[2], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));

            let lookup = LookupBuf::from_str("root[1]")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"].as_array()[1], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));

            let lookup = LookupBuf::from_str("root[0]")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"].as_array()[0], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }

        #[test_env_log::test]
        fn array_field_nested_array() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root[0][0]")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(event.inner()["root"].as_array()[0].as_array()[0], value);
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }

        #[test_env_log::test]
        fn array_field_nested_map() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str("root[0].nested")?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(
                event.inner()["root"].as_array()[0].as_map()["nested"],
                value
            );
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }

        #[test_env_log::test]
        fn perverse() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookup = LookupBuf::from_str(
                "root[10].nested[10].more[9].than[8].there[7][6][5].we.go.friends.look.at.this",
            )?;
            let mut value = Value::Boolean(true);
            event.insert(lookup.clone(), value.clone());
            assert_eq!(
                event.inner()["root"].as_array()[10].as_map()["nested"].as_array()[10].as_map()
                    ["more"]
                    .as_array()[9]
                    .as_map()["than"]
                    .as_array()[8]
                    .as_map()["there"]
                    .as_array()[7]
                    .as_array()[6]
                    .as_array()[5]
                    .as_map()["we"]
                    .as_map()["go"]
                    .as_map()["friends"]
                    .as_map()["look"]
                    .as_map()["at"]
                    .as_map()["this"],
                value
            );
            assert_eq!(event.get(&lookup), Some(&value));
            assert_eq!(event.get_mut(&lookup), Some(&mut value));
            assert_eq!(event.remove(&lookup, false), Some(value));
            Ok(())
        }
    }

    mod corner_cases {
        use super::*;

        // Prune on deeply nested values is tested in `value.rs`, but we must test root values here.
        #[test_env_log::test]
        fn pruning() -> crate::Result<()> {

            let mut event = crate::log_event! {
                LookupBuf::from("foo.bar.baz") => 1,
            }
            .into_log();
            assert_eq!(
                event.remove(Lookup::from("foo.bar.baz"), true),
                Some(Value::from(1))
            );
            assert!(!event.contains(Lookup::from("foo.bar")));
            assert!(!event.contains(Lookup::from("foo")));

            let mut event = crate::log_event! {
                LookupBuf::from("foo.bar") => 1,
            }
            .into_log();
            assert_eq!(
                event.remove(Lookup::from("foo.bar"), true),
                Some(Value::from(1))
            );
            assert!(!event.contains(Lookup::from("foo")));

            Ok(())
        }

        // While authors should prefer to set an array via `event.insert(lookup_to_array, array)`,
        // there are some cases where we want to insert 1 by one. Make sure this can happen.
        #[test_env_log::test]
        fn iteratively_populate_array() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookups = vec![
                LookupBuf::from_str("root.nested[0]")?,
                LookupBuf::from_str("root.nested[1]")?,
                LookupBuf::from_str("root.nested[2]")?,
                LookupBuf::from_str("other[1][0]")?,
                LookupBuf::from_str("other[1][1].a")?,
                LookupBuf::from_str("other[1][1].b")?,
            ];
            let value = Value::Boolean(true);
            for lookup in lookups.clone() {
                event.insert(lookup, value.clone());
            }
            let pairs = event.keys(true).collect::<Vec<_>>();
            for lookup in lookups {
                assert!(
                    pairs.contains(&lookup.clone_lookup()),
                    "Failed while looking for {}",
                    lookup
                );
            }
            Ok(())
        }

        // While authors should prefer to set an array via `event.insert(lookup_to_array, array)`,
        // there are some cases where we want to insert 1 by one. Make sure this can happen.
        #[test_env_log::test]
        fn iteratively_populate_array_reverse() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookups = vec![
                LookupBuf::from_str("root.nested[1]")?,
                LookupBuf::from_str("root.nested[0]")?,
                LookupBuf::from_str("other[1][1]")?,
                LookupBuf::from_str("other[0][1].a")?,
            ];
            let value = Value::Boolean(true);
            for lookup in lookups.clone() {
                event.insert(lookup, value.clone());
            }
            let pairs = event.keys(false).collect::<Vec<_>>();
            for lookup in lookups.clone() {
                assert!(
                    pairs.contains(&lookup.clone_lookup()),
                    "Failed while looking for {} in {}",
                    lookup,
                    pairs
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<String>>()
                        .join(", ")
                );
            }
            Ok(())
        }

        // While authors should prefer to set an map via `event.insert(lookup_to_map, map)`,
        // there are some cases where we want to insert 1 by one. Make sure this can happen.
        #[test_env_log::test]
        fn iteratively_populate_map() -> crate::Result<()> {
            let mut event = LogEvent::default();
            let lookups = vec![
                LookupBuf::from_str("root.one")?,
                LookupBuf::from_str("root.two")?,
                LookupBuf::from_str("root.three.a")?,
                LookupBuf::from_str("root.three.b")?,
                LookupBuf::from_str("root.three.c")?,
                LookupBuf::from_str("root.four[0]")?,
                LookupBuf::from_str("root.four[1]")?,
                LookupBuf::from_str("root.four[2]")?,
            ];
            let value = Value::Boolean(true);
            for lookup in lookups.clone() {
                event.insert(lookup, value.clone());
            }
            // Note: Two Lookups are only the same if the string slices underneath are too.
            //       LookupBufs this rule does not apply.
            let pairs = event.keys(true).map(|k| k.into_buf()).collect::<Vec<_>>();
            for lookup in lookups {
                assert!(
                    pairs.contains(&lookup),
                    "Failed while looking for {}",
                    lookup
                );
            }
            Ok(())
        }
    }

    #[test_env_log::test]
    fn keys_and_pairs() -> crate::Result<()> {
        let mut event = LogEvent::default();
        // We opt for very small arrays here to avoid having to iterate a bunch.
        let lookup = LookupBuf::from_str("snooper.booper[1][2]")?;
        event.insert(lookup, Value::Null);
        let lookup = LookupBuf::from_str("whomp[1].glomp[1]")?;
        event.insert(lookup, Value::Null);
        let lookup = LookupBuf::from_str("zoop")?;
        event.insert(lookup, Value::Null);

        // Collect and sort since we don't want a flaky test on iteration do we?
        let mut keys = event.keys(false).collect::<Vec<_>>();
        keys.sort();
        let mut pairs = event.pairs(false).collect::<Vec<_>>();
        pairs.sort_by(|v, x| v.0.cmp(&x.0));

        // Ensure a new field element that was injected is iterated over.
        let expected = Lookup::from_str("snooper").unwrap();
        assert_eq!(keys[0], expected);
        assert_eq!(pairs[0].0, expected);
        let expected = Lookup::from_str("snooper.booper").unwrap();
        assert_eq!(keys[1], expected);
        assert_eq!(pairs[1].0, expected);
        // Ensure a new array element that was injected is iterated over.
        let expected = Lookup::from_str("snooper.booper[0]").unwrap();
        assert_eq!(keys[2], expected);
        assert_eq!(pairs[2].0, expected);
        let expected = Lookup::from_str("snooper.booper[1]").unwrap();
        assert_eq!(keys[3], expected);
        assert_eq!(pairs[3].0, expected);
        let expected = Lookup::from_str("snooper.booper[1][0]").unwrap();
        assert_eq!(keys[4], expected);
        assert_eq!(pairs[4].0, expected);
        let expected = Lookup::from_str("snooper.booper[1][1]").unwrap();
        assert_eq!(keys[5], expected);
        assert_eq!(pairs[5].0, expected);
        let expected = Lookup::from_str("snooper.booper[1][2]").unwrap();
        assert_eq!(keys[6], expected);
        assert_eq!(pairs[6].0, expected);
        // Try inside arrays now.
        let expected = Lookup::from_str("whomp").unwrap();
        assert_eq!(keys[7], expected);
        assert_eq!(pairs[7].0, expected);
        let expected = Lookup::from_str("whomp[0]").unwrap();
        assert_eq!(keys[8], expected);
        assert_eq!(pairs[8].0, expected);
        let expected = Lookup::from_str("whomp[1]").unwrap();
        assert_eq!(keys[9], expected);
        assert_eq!(pairs[9].0, expected);
        let expected = Lookup::from_str("whomp[1].glomp").unwrap();
        assert_eq!(keys[10], expected);
        assert_eq!(pairs[10].0, expected);
        let expected = Lookup::from_str("whomp[1].glomp[0]").unwrap();
        assert_eq!(keys[11], expected);
        assert_eq!(pairs[11].0, expected);
        let expected = Lookup::from_str("whomp[1].glomp[1]").unwrap();
        assert_eq!(keys[12], expected);
        assert_eq!(pairs[12].0, expected);
        let expected = Lookup::from_str("zoop").unwrap();
        assert_eq!(keys[13], expected);
        assert_eq!(pairs[13].0, expected);

        Ok(())
    }
}