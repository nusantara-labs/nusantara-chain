/// A staged write batch that accumulates operations before atomic commit.
pub struct StorageWriteBatch {
    pub(crate) ops: Vec<BatchOp>,
}

pub(crate) enum BatchOp {
    Put {
        cf: &'static str,
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Delete {
        cf: &'static str,
        key: Vec<u8>,
    },
}

impl StorageWriteBatch {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    pub fn put(&mut self, cf: &'static str, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) {
        self.ops.push(BatchOp::Put {
            cf,
            key: key.into(),
            value: value.into(),
        });
    }

    pub fn delete(&mut self, cf: &'static str, key: impl Into<Vec<u8>>) {
        self.ops.push(BatchOp::Delete {
            cf,
            key: key.into(),
        });
    }

    /// Merge all operations from another batch into this one (cloning).
    pub fn merge(&mut self, other: &StorageWriteBatch) {
        self.ops.extend(other.ops.iter().map(|op| match op {
            BatchOp::Put { cf, key, value } => BatchOp::Put {
                cf,
                key: key.clone(),
                value: value.clone(),
            },
            BatchOp::Delete { cf, key } => BatchOp::Delete {
                cf,
                key: key.clone(),
            },
        }));
    }

    /// Merge all operations from another batch by moving (zero-copy).
    pub fn merge_owned(&mut self, other: StorageWriteBatch) {
        self.ops.extend(other.ops);
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }
}

impl Default for StorageWriteBatch {
    fn default() -> Self {
        Self::new()
    }
}
