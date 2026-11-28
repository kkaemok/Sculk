use crate::{Type, Value};
use std::{collections::BTreeMap, sync::Mutex};

const CAPACITY: usize = 256;
const STATE_BYTES: usize = 128 * 1024;
const VALUE_BYTES: usize = 16 * 1024;

/// Shared counters and typed state. Individual reads and mutations are atomic.
/// A sequence of script statements can interleave with another handler.
#[derive(Default)]
pub struct GlobalCounters {
    values: Mutex<State>,
}

#[derive(Default)]
struct State {
    counters: BTreeMap<String, i64>,
    variables: BTreeMap<String, Value>,
    bytes: usize,
}

/// Checked state used by both in-memory and durable hosts.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct StoredState {
    pub counters: Vec<(String, i64)>,
    pub variables: Vec<(String, Value)>,
}

/// A snapshot violated state bounds, or the in-memory store failed.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("invalid or unavailable script state")]
pub struct CounterError;

pub(crate) fn valid_name(name: &str) -> bool {
    name.len() <= 128
        && name.split("::").all(|part| {
            !part.is_empty()
                && part.len() <= 64
                && !part.starts_with('_')
                && part.chars().all(|c| c.is_alphanumeric() || c == '_')
        })
}

pub(crate) fn valid_script_name(name: &str) -> bool {
    // Leave room for the exact scoped Int64 and separators in the durable key.
    name.len() <= 104 && valid_name(name)
}

pub(crate) fn value_size(value: &Value) -> Option<usize> {
    match value {
        Value::String(text) => (text.len() <= 4096).then_some(text.len() + 32),
        Value::Integer(_) | Value::Boolean(_) => Some(32),
        Value::Collection(items) => items.storage_size(),
    }
}

impl GlobalCounters {
    /// Restore the legacy counter snapshot format.
    pub fn from_snapshot(
        entries: impl IntoIterator<Item = (String, i64)>,
    ) -> Result<Self, CounterError> {
        let counters = entries.into_iter().take(CAPACITY + 1).collect();
        Self::from_state(StoredState {
            counters,
            variables: Vec::new(),
        })
    }

    /// Validate length, names, uniqueness and types before restoring any state.
    pub fn from_state(snapshot: StoredState) -> Result<Self, CounterError> {
        if snapshot
            .counters
            .len()
            .saturating_add(snapshot.variables.len())
            > CAPACITY
        {
            return Err(CounterError);
        }
        let mut values = State::default();
        for (name, value) in snapshot.counters {
            if !valid_name(&name) || values.counters.insert(name, value).is_some() {
                return Err(CounterError);
            }
        }
        for (name, value) in snapshot.variables {
            if values.variables.contains_key(&name) {
                return Err(CounterError);
            }
            values.set(&name, value)?;
        }
        Ok(Self {
            values: Mutex::new(values),
        })
    }

    /// Legacy counter-only snapshot.
    pub fn snapshot(&self) -> Result<Vec<(String, i64)>, CounterError> {
        let values = self.values.lock().map_err(|_| CounterError)?;
        Ok(values
            .counters
            .iter()
            .map(|(name, value)| (name.clone(), *value))
            .collect())
    }

    /// One atomic snapshot of the full state for a trusted host.
    pub fn snapshot_state(&self) -> Result<StoredState, CounterError> {
        let values = self.values.lock().map_err(|_| CounterError)?;
        Ok(StoredState {
            counters: values
                .counters
                .iter()
                .map(|(name, value)| (name.clone(), *value))
                .collect(),
            variables: values
                .variables
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
        })
    }

    pub(crate) fn get(&self, name: &str) -> Result<i64, CounterError> {
        Ok(self
            .values
            .lock()
            .map_err(|_| CounterError)?
            .counters
            .get(name)
            .copied()
            .unwrap_or(0))
    }

    pub(crate) fn contains(&self, name: &str) -> Result<bool, CounterError> {
        Ok(self
            .values
            .lock()
            .map_err(|_| CounterError)?
            .counters
            .contains_key(name))
    }

    pub(crate) fn update(
        &self,
        name: &str,
        update: impl FnOnce(i64) -> Result<i64, crate::RuntimeError>,
    ) -> Result<(), crate::RuntimeError> {
        let mut values = self.values.lock().map_err(|_| crate::RuntimeError::State)?;
        if let Some(value) = values.counters.get_mut(name) {
            *value = update(*value)?;
        } else {
            if values.counters.len() + values.variables.len() >= CAPACITY {
                return Err(crate::RuntimeError::State);
            }
            values.counters.insert(name.into(), update(0)?);
        }
        Ok(())
    }

    pub(crate) fn delete(&self, name: &str) -> Result<(), CounterError> {
        self.values
            .lock()
            .map_err(|_| CounterError)?
            .counters
            .remove(name);
        Ok(())
    }

    pub(crate) fn get_state(&self, name: &str, ty: Type) -> Result<Option<Value>, CounterError> {
        let values = self.values.lock().map_err(|_| CounterError)?;
        match values.variables.get(name) {
            Some(value) if value.ty() != ty => Err(CounterError),
            value => Ok(value.cloned()),
        }
    }

    pub(crate) fn set_state(&self, name: &str, value: Value) -> Result<(), CounterError> {
        self.values
            .lock()
            .map_err(|_| CounterError)?
            .set(name, value)
    }

    pub(crate) fn delete_state(&self, name: &str, ty: Type) -> Result<(), CounterError> {
        let mut values = self.values.lock().map_err(|_| CounterError)?;
        if let Some(value) = values.variables.get(name) {
            if value.ty() != ty {
                return Err(CounterError);
            }
            let bytes = value_size(value).ok_or(CounterError)?;
            values.variables.remove(name);
            values.bytes -= bytes;
        }
        Ok(())
    }
}

impl State {
    fn set(&mut self, name: &str, value: Value) -> Result<(), CounterError> {
        if !valid_name(name) {
            return Err(CounterError);
        }
        let size = value_size(&value)
            .filter(|size| *size <= VALUE_BYTES)
            .ok_or(CounterError)?;
        let old = match self.variables.get(name) {
            Some(old) if old.ty() == value.ty() => value_size(old).ok_or(CounterError)?,
            Some(_) => return Err(CounterError),
            None if self.variables.len() + self.counters.len() < CAPACITY => 0,
            None => return Err(CounterError),
        };
        let bytes = self.bytes - old + size;
        if bytes > STATE_BYTES {
            return Err(CounterError);
        }
        self.variables.insert(name.to_owned(), value);
        self.bytes = bytes;
        Ok(())
    }
}
