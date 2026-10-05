//! One async-loaded value. The last good value survives a failed or pending
//! reload, so a refresh never blanks the screen; every load gets a generation,
//! and a result whose generation is no longer current is dropped rather than
//! cancelled.

use crate::framework::cx::Cx;

#[derive(Debug)]
pub struct Slot<T> {
    pub data: Option<T>,
    pub loading: bool,
    /// The last load's error, shown above stale data.
    pub error: Option<String>,
    /// Unix seconds of the last successful load.
    pub loaded_at: Option<u64>,
    generation: u64,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot {
            data: None,
            loading: false,
            error: None,
            loaded_at: None,
            generation: 0,
        }
    }
}

impl<T> Slot<T> {
    /// Starts a load; pass the returned generation back to [`Slot::finish`].
    pub fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.loading = true;
        self.generation
    }

    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    /// Applies a result if it is current. Returns the error, if any, for the
    /// caller to inspect (a signed-out session is everyone's problem); a stale
    /// result returns `None` and changes nothing.
    pub fn finish<E: std::fmt::Display>(
        &mut self,
        generation: u64,
        result: Result<T, E>,
    ) -> Option<E> {
        if generation != self.generation {
            return None;
        }
        self.loading = false;
        match result {
            Ok(data) => {
                self.data = Some(data);
                self.error = None;
                self.loaded_at = Some(now_unix());
                None
            }
            Err(err) => {
                self.error = Some(err.to_string());
                Some(err)
            }
        }
    }

    /// Spawns `fut` as a load of this slot: the message it produces is built
    /// by `wrap(generation, result)` and comes back to the module.
    pub fn load<M, E, F>(
        &mut self,
        cx: &Cx<'_>,
        fut: F,
        wrap: impl FnOnce(u64, Result<T, E>) -> M + Send + 'static,
    ) where
        M: std::any::Any + Send,
        T: Send + 'static,
        E: Send + 'static,
        F: std::future::Future<Output = Result<T, E>> + Send + 'static,
    {
        let generation = self.begin();
        cx.spawn(async move { wrap(generation, fut.await) });
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_results_are_dropped_and_data_survives_errors() {
        let mut slot: Slot<u32> = Slot::default();
        let first = slot.begin();
        let second = slot.begin();
        assert!(slot.finish::<String>(first, Ok(1)).is_none());
        assert!(
            slot.data.is_none() && slot.loading,
            "the stale result changed nothing"
        );
        assert!(slot.finish::<String>(second, Ok(2)).is_none());
        assert_eq!(slot.data, Some(2));
        let third = slot.begin();
        assert_eq!(
            slot.finish(third, Err::<u32, _>("boom".to_string())),
            Some("boom".into())
        );
        assert_eq!(slot.data, Some(2));
        assert_eq!(slot.error.as_deref(), Some("boom"));
        assert!(!slot.loading);
    }
}
