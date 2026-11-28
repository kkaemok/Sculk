use crate::{RuntimeError, Type, Value};
use std::{collections::BTreeMap, sync::Arc};

const CAPACITY: usize = 128;

/// Element type of a homogeneous list or string-keyed map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scalar {
    Text,
    Integer,
    Boolean,
}
impl Scalar {
    pub(crate) fn ty(self) -> Type {
        match self {
            Self::Text => Type::String,
            Self::Integer => Type::Integer,
            Self::Boolean => Type::Boolean,
        }
    }
}

/// An immutable, cheaply cloned list or map. Script updates return a new value.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Collection {
    scalar: Scalar,
    items: Items,
}
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Items {
    List(Arc<[Value]>),
    Map(Arc<BTreeMap<Arc<str>, Value>>),
}
impl Collection {
    pub(crate) fn storage_size(&self) -> Option<usize> {
        if self.len() > CAPACITY {
            return None;
        }
        let size = |value: &Value| {
            if value.ty() != self.scalar.ty() {
                return None;
            }
            crate::globals::value_size(value)
        };
        match &self.items {
            Items::List(items) => items
                .iter()
                .try_fold(0, |total, value| Some(total + size(value)?)),
            Items::Map(items) => items.iter().try_fold(0, |total, (key, value)| {
                (key.len() <= 256).then_some(total + key.len() + size(value)?)
            }),
        }
        .map(|total| total + 32)
    }
    pub(crate) fn ty(&self) -> Type {
        match self.items {
            Items::List(_) => Type::List(self.scalar),
            Items::Map(_) => Type::Map(self.scalar),
        }
    }

    pub(crate) fn len(&self) -> usize {
        match &self.items {
            Items::List(items) => items.len(),
            Items::Map(items) => items.len(),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Builtin {
    List(Scalar),
    Map(Scalar),
    Length,
    Item,
    Contains,
    Append,
    Remove,
    Get,
    HasKey,
    Put,
    RemoveKey,
    KeyAt,
    ValueAt,
}

impl Builtin {
    pub(crate) fn named(name: &str) -> Option<Self> {
        Some(match name {
            "text-list" => Self::List(Scalar::Text),
            "integer-list" => Self::List(Scalar::Integer),
            "boolean-list" => Self::List(Scalar::Boolean),
            "text-map" => Self::Map(Scalar::Text),
            "integer-map" => Self::Map(Scalar::Integer),
            "boolean-map" => Self::Map(Scalar::Boolean),
            "length" => Self::Length,
            "item" => Self::Item,
            "contains" => Self::Contains,
            "append" => Self::Append,
            "remove" => Self::Remove,
            "get" => Self::Get,
            "has-key" => Self::HasKey,
            "put" => Self::Put,
            "remove-key" => Self::RemoveKey,
            "key-at" => Self::KeyAt,
            "value-at" => Self::ValueAt,
            _ => return None,
        })
    }

    pub(crate) fn result(self, args: &[Type]) -> Option<Type> {
        use Type::{Boolean, Integer, List, Map, String};
        Some(match (self, args) {
            (Self::List(scalar), args) if args.iter().all(|arg| *arg == scalar.ty()) => {
                List(scalar)
            }
            (Self::Map(scalar), []) => Map(scalar),
            (Self::Length, [List(_) | Map(_)]) => Integer,
            (Self::Item, [List(scalar), Integer]) | (Self::ValueAt, [Map(scalar), Integer]) => {
                scalar.ty()
            }
            (Self::Append, [List(scalar), value]) if *value == scalar.ty() => List(*scalar),
            (Self::Contains, [List(scalar), value]) if *value == scalar.ty() => Boolean,
            (Self::Remove, [List(scalar), Integer]) => List(*scalar),
            (Self::Get, [Map(scalar), String]) => scalar.ty(),
            (Self::HasKey, [Map(_), String]) => Boolean,
            (Self::Put, [Map(scalar), String, value]) if *value == scalar.ty() => Map(*scalar),
            (Self::RemoveKey, [Map(scalar), String]) => Map(*scalar),
            (Self::KeyAt, [Map(_), Integer]) => String,
            _ => return None,
        })
    }

    pub(crate) fn run(self, args: Vec<Value>) -> Result<Value, RuntimeError> {
        match self {
            Self::List(scalar) => {
                return Ok(Value::Collection(Collection {
                    scalar,
                    items: Items::List(args.into()),
                }))
            }
            Self::Map(scalar) => {
                return Ok(Value::Collection(Collection {
                    scalar,
                    items: Items::Map(Arc::default()),
                }))
            }
            _ => {}
        }
        let Some((Value::Collection(collection), args)) = args.split_first() else {
            return Err(RuntimeError::Invalid);
        };
        let scalar = collection.scalar;
        if matches!(self, Self::Length) {
            return Ok(Value::Integer(collection.len() as i64));
        }
        let index = |value: &Value| -> Result<usize, RuntimeError> {
            value
                .as_integer()
                .and_then(|index| usize::try_from(index).ok())
                .filter(|index| *index < collection.len())
                .ok_or(RuntimeError::CollectionIndex)
        };
        let items = match (&collection.items, self, args) {
            (Items::List(items), Self::Item, [value]) => return Ok(items[index(value)?].clone()),
            (Items::List(items), Self::Contains, [value]) => {
                return Ok(Value::Boolean(items.contains(value)))
            }
            (Items::List(items), Self::Append, [value]) => {
                if items.len() == CAPACITY {
                    return Err(RuntimeError::CollectionLimit);
                }
                let mut items = items.to_vec();
                items.push(value.clone());
                Items::List(items.into())
            }
            (Items::List(items), Self::Remove, [value]) => {
                let mut items = items.to_vec();
                items.remove(index(value)?);
                Items::List(items.into())
            }
            (Items::Map(items), Self::Get, [Value::String(key)]) => {
                return items.get(key).cloned().ok_or(RuntimeError::CollectionIndex);
            }
            (Items::Map(items), Self::HasKey, [Value::String(key)]) => {
                return Ok(Value::Boolean(items.contains_key(key)))
            }
            (Items::Map(items), Self::KeyAt, [value]) => {
                let key = items
                    .keys()
                    .nth(index(value)?)
                    .ok_or(RuntimeError::Invalid)?;
                return Ok(Value::String(key.clone()));
            }
            (Items::Map(items), Self::ValueAt, [value]) => {
                return items
                    .values()
                    .nth(index(value)?)
                    .cloned()
                    .ok_or(RuntimeError::Invalid);
            }
            (Items::Map(items), Self::Put, [Value::String(key), value]) => {
                if key.len() > CAPACITY || (items.len() == CAPACITY && !items.contains_key(key)) {
                    return Err(RuntimeError::CollectionLimit);
                }
                let mut items = (**items).clone();
                items.insert(key.clone(), value.clone());
                Items::Map(Arc::new(items))
            }
            (Items::Map(items), Self::RemoveKey, [Value::String(key)]) => {
                let mut items = (**items).clone();
                items.remove(key);
                Items::Map(Arc::new(items))
            }
            _ => return Err(RuntimeError::Invalid),
        };
        Ok(Value::Collection(Collection { scalar, items }))
    }
}
