use revm::{database::CacheState, DatabaseRef};

pub struct CachedDb<'a, DB: DatabaseRef> {
    pub db: &'a DB,
    pub cache: CacheState,
}

impl<'a, DB: DatabaseRef> CachedDb<'a, DB> {
    pub fn new(db: &'a DB, cache: CacheState) -> Self {
        CachedDb { db, cache }
    }
}
