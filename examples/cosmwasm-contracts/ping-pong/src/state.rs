use cw_storage_plus::Item;

pub const PINGS_SENT: Item<u64> = Item::new("pings_sent");
pub const PONGS_RECEIVED: Item<u64> = Item::new("pongs_received");
pub const NEXT_SEQUENCE: Item<u64> = Item::new("next_sequence");
