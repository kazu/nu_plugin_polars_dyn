mod list;
mod rm;

use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
};

use chrono::{DateTime, FixedOffset, Local};
pub use list::ListDF;
use nu_plugin::{EngineInterface, PluginCommand};
use nu_protocol::{ShellError, Span, shell_error::generic::GenericError};
use uuid::Uuid;

use crate::{PolarsPlugin, values::PolarsPluginObject};

use log::debug;

#[derive(Debug, Clone)]
pub struct CacheValue {
    pub uuid: Uuid,
    pub value: PolarsPluginObject,
    pub created: DateTime<FixedOffset>,
    pub span: Span,
    /// How many values carrying this id the plugin has handed to the engine and has not been
    /// told about the drop of. The engine collapses its own clones, so it notifies once per
    /// value the plugin sent.
    pub reference_count: i16,
}

#[derive(Default)]
pub struct Cache {
    cache: Mutex<HashMap<Uuid, CacheValue>>,
}

impl Cache {
    fn lock(&self) -> Result<MutexGuard<'_, HashMap<Uuid, CacheValue>>, ShellError> {
        self.cache.lock().map_err(|e| {
            ShellError::Generic(GenericError::new_internal(
                format!("error acquiring cache lock: {e}"),
                "",
            ))
        })
    }

    /// Drops one reference to an entry, and removes it once the last one is gone.
    ///
    /// * `key` - The key of the cache entry to remove.
    /// * `force` - Remove the entry even if references are left.
    pub fn remove(&self, key: &Uuid, force: bool) -> Result<Option<CacheValue>, ShellError> {
        let mut lock = self.lock()?;

        let reference_count = lock.get_mut(key).map(|cache_value| {
            cache_value.reference_count -= 1;
            cache_value.reference_count
        });

        let removed = if force || reference_count.unwrap_or_default() < 1 {
            let removed = lock.remove(key);
            debug!("PolarsPlugin: removing {key} from cache: {removed:?}");
            removed
        } else {
            debug!("PolarsPlugin: decrementing reference count for {key}");
            None
        };

        drop(lock);
        Ok(removed)
    }

    /// Inserts an item into the plugin cache, counting one reference for the value the caller is
    /// about to hand to the engine. An id that is already cached is counted again rather than
    /// reset, since the caller is handing out a second value for it.
    pub fn insert(
        &self,
        uuid: Uuid,
        value: PolarsPluginObject,
        span: Span,
    ) -> Result<Option<CacheValue>, ShellError> {
        let mut lock = self.lock()?;
        debug!("PolarsPlugin: Inserting {uuid} into cache: {value:?}");
        let reference_count = lock
            .get(&uuid)
            .map(|cache_value| cache_value.reference_count + 1)
            .unwrap_or(1);
        let cache_value = CacheValue {
            uuid,
            value,
            created: Local::now().into(),
            span,
            reference_count,
        };
        let result = lock.insert(uuid, cache_value);
        drop(lock);
        Ok(result)
    }

    /// Reads an entry. `increment` counts one more reference, and is for callers that hand the
    /// cached value to the engine again without going through [`Cache::insert`].
    pub fn get(&self, uuid: &Uuid, increment: bool) -> Result<Option<CacheValue>, ShellError> {
        let mut lock = self.lock()?;
        let result = lock.get_mut(uuid).map(|cv| {
            if increment {
                cv.reference_count += 1;
            }
            cv.clone()
        });
        drop(lock);
        Ok(result)
    }

    pub fn process_entries<F, T>(&self, mut func: F) -> Result<Vec<T>, ShellError>
    where
        F: FnMut((&Uuid, &CacheValue)) -> Result<T, ShellError>,
    {
        let lock = self.lock()?;
        let mut vals: Vec<T> = Vec::new();
        for entry in lock.iter() {
            let val = func(entry)?;
            vals.push(val);
        }
        drop(lock);
        Ok(vals)
    }
}

pub trait Cacheable: Sized + Clone {
    fn cache_id(&self) -> &Uuid;

    fn to_cache_value(&self) -> Result<PolarsPluginObject, ShellError>;

    fn from_cache_value(cv: PolarsPluginObject) -> Result<Self, ShellError>;

    fn cache(
        self,
        plugin: &PolarsPlugin,
        engine: &EngineInterface,
        span: Span,
    ) -> Result<Self, ShellError> {
        plugin.disable_gc_once(engine)?;
        plugin
            .cache
            .insert(self.cache_id().to_owned(), self.to_cache_value()?, span)?;
        Ok(self)
    }

    fn get_cached(plugin: &PolarsPlugin, id: &Uuid) -> Result<Option<Self>, ShellError> {
        if let Some(cache_value) = plugin.cache.get(id, false)? {
            Ok(Some(Self::from_cache_value(cache_value.value)?))
        } else {
            Ok(None)
        }
    }
}

pub(crate) fn cache_commands() -> Vec<Box<dyn PluginCommand<Plugin = PolarsPlugin>>> {
    vec![Box::new(ListDF), Box::new(rm::CacheRemove)]
}
