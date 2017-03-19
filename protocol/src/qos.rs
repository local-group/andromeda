#[repr(u8)]
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Copy, Clone, Hash)]
pub enum QualityOfService {
    Level0 = 0,
    Level1 = 1,
    Level2 = 2,
}
