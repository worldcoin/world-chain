use alloy_rpc_types_engine::PayloadId;
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
    time::Instant,
};

/// Weak entries do not extend a job or its detached workers' resource leases.
#[derive(Clone, Debug, Default)]
pub struct SigningDeadlines(Arc<Mutex<HashMap<PayloadId, Weak<Instant>>>>);

impl SigningDeadlines {
    pub fn register(&self, id: PayloadId, until: Instant) -> Result<Arc<Instant>, &'static str> {
        let mut jobs = self.0.lock();
        jobs.retain(|_, value| value.strong_count() > 0);
        if jobs.contains_key(&id) {
            return Err("a signing budget for this payload is still active");
        }
        let lease = Arc::new(until);
        jobs.insert(id, Arc::downgrade(&lease));
        Ok(lease)
    }

    pub fn get(&self, id: PayloadId) -> Option<Instant> {
        self.0
            .lock()
            .get(&id)
            .and_then(Weak::upgrade)
            .map(|value| *value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detached_workers_keep_the_original_budget_without_allowing_id_reuse() {
        let budgets = SigningDeadlines::default();
        let id = PayloadId::default();
        let until = Instant::now();
        let job = budgets.register(id, until).expect("new job");
        let worker = job.clone();
        drop(job);
        assert_eq!(budgets.get(id), Some(until));
        assert!(budgets.register(id, until).is_err());
        drop(worker);
        assert_eq!(budgets.get(id), None);
        assert!(budgets.register(id, until).is_ok());
    }
}
