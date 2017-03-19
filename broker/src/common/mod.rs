
use mqtt::{TopicName, TopicFilter};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Topic {
    Filter(TopicFilter),
    Name(TopicName)
}

impl Topic {

    pub fn from_filter(filter: &TopicFilter) -> Topic {
        if (*filter).find('+').is_some() || (*filter).find('#').is_some() {
            Topic::Filter(filter.clone())
        } else {
            unsafe { Topic::Name(TopicName::new_unchecked((*filter).to_string())) }
        }
    }

    pub fn from_name(name: &TopicName) -> Topic {
        Topic::Name(name.clone())
    }

    pub fn from_str(topic: &str) -> Topic {
        if topic.find('+').is_some() || topic.find('#').is_some() {
            Topic::Filter(TopicFilter::new(topic))
        } else {
            Topic::Name(TopicName::new(topic.to_string()).unwrap())
        }
    }
}
