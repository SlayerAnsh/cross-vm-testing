use cosmwasm_std::Event;

pub const PING_MSG: &str = "ping";
pub const PONG_ACK: &str = "pong";

pub fn send_packet_event(source: &str, dest: &str, seq: u64, msg: &str) -> Event {
    Event::new("SendPacket")
        .add_attribute("source_port", source)
        .add_attribute("destination_port", dest)
        .add_attribute("packet_sequence", seq.to_string())
        .add_attribute("msg", msg)
}

pub fn receive_packet_event(source: &str, dest: &str, seq: u64) -> Event {
    Event::new("ReceivePacket")
        .add_attribute("source_port", source)
        .add_attribute("destination_port", dest)
        .add_attribute("packet_sequence", seq.to_string())
}

pub fn write_acknowledgement_event(source: &str, dest: &str, seq: u64, msg: &str, ack: &str) -> Event {
    Event::new("WriteAcknowledgement")
        .add_attribute("source_port", source)
        .add_attribute("destination_port", dest)
        .add_attribute("packet_sequence", seq.to_string())
        .add_attribute("msg", msg)
        .add_attribute("ack", ack)
}

pub fn acknowledge_packet_event(source: &str, dest: &str, seq: u64) -> Event {
    Event::new("AcknowledgePacket")
        .add_attribute("source_port", source)
        .add_attribute("destination_port", dest)
        .add_attribute("packet_sequence", seq.to_string())
}
