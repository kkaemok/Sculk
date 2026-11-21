use std::{collections::BTreeMap, sync::Mutex};

const CAPACITY: usize = 256;

/// Shared integer counters. Each read, assignment, increment or deletion is
/// atomic. A sequence of script statements can interleave with another handler.
#[derive(Default)]
pub struct GlobalCounters {
    values: Mutex<BTreeMap<String, i64>>,
}

/// A snapshot violated counter bounds, or the in-memory store failed.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("invalid or unavailable script counter state")]
pub struct CounterError;

pub(crate) fn valid_name(name: &str) -> bool {
    !name.starts_with('_')
        && !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

impl GlobalCounters {
    /// Restore a checked snapshot. Duplicate or invalid names fail the entire load.
    pub fn from_snapshot(
        entries: impl IntoIterator<Item = (String, i64)>,
    ) -> Result<Self, CounterError> {
        let mut values = BTreeMap::new();
        for (name, value) in entries {
            if values.len() == CAPACITY
                || !valid_name(&name)
                || values.insert(name, value).is_some()
            {
                return Err(CounterError);
            }
        }
        Ok(Self {
            values: Mutex::new(values),
        })
    }

    /// Consistent sorted snapshot for a trusted host to save outside dispatch.
    pub fn snapshot(&self) -> Result<Vec<(String, i64)>, CounterError> {
        let values = self.values.lock().map_err(|_| CounterError)?;
        Ok(values
            .iter()
            .map(|(name, value)| (name.clone(), *value))
            .collect())
    }

    pub(crate) fn get(&self, name: &str) -> Result<i64, CounterError> {
        Ok(self
            .values
            .lock()
            .map_err(|_| CounterError)?
            .get(name)
            .copied()
            .unwrap_or(0))
    }

    pub(crate) fn update(
        &self,
        name: &str,
        update: impl FnOnce(i64) -> Result<i64, crate::RuntimeError>,
    ) -> Result<(), crate::RuntimeError> {
        let mut values = self.values.lock().map_err(|_| crate::RuntimeError::State)?;
        if let Some(value) = values.get_mut(name) {
            *value = update(*value)?;
        } else {
            if values.len() >= CAPACITY {
                return Err(crate::RuntimeError::State);
            }
            values.insert(name.into(), update(0)?);
        }
        Ok(())
    }

    pub(crate) fn delete(&self, name: &str) -> Result<(), CounterError> {
        self.values.lock().map_err(|_| CounterError)?.remove(name);
        Ok(())
    }
}
