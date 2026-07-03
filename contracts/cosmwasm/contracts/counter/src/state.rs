use cw_storage_plus::Item;

pub const COUNTER: Item<u64> = Item::new("counter");
pub const VERSION: Item<u64> = Item::new("version");
