
use mqtt::{TopicName};
use mqtt::packet::{PublishPacket, QoSWithPacketIdentifier};

use broker::common::{Topic};
use broker::store::global_retain::RetainNode;

#[test]
fn test_retain_node_simple() {
    let mut retains = RetainNode::new();
    for topic_name in vec!["/a/b", "/a", "a/b/c"] {
        retains.insert(&PublishPacket::new(
            TopicName::new(topic_name.to_string()).unwrap(),
            QoSWithPacketIdentifier::Level0,
            Vec::new()))
    }

    println!("[Retains]: {:#?}", retains);
    let cases = vec![("/a/b", 1),
                     ("/a/+", 1),
                     ("/+", 1),
                     ("/#", 2),
                     ("/a/#", 1)];
    for &(topic, length) in &cases {
        println!("========================================(topic={}, length={})", topic, length);
        assert_eq!(retains.match_all(&Topic::from_str(topic)).len(), length);
    }

    retains.remove("/a/b");
    let cases = vec![("/a/b", 0),
                     ("/a/+", 0),
                     ("/+", 1),
                     ("/#", 1),
                     ("/a/#", 0)];
    for &(topic, length) in &cases {
        println!("========================================(topic={}, length={})", topic, length);
        assert_eq!(retains.match_all(&Topic::from_str(topic)).len(), length);
    }

    assert_eq!(retains.is_empty(), false);
    retains.remove("/a");
    retains.remove("a/b/c");
    println!("[Retains]: {:#?}", retains);
    assert_eq!(retains.is_empty(), true);
}
