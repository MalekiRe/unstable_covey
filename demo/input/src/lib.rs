use static_ecs::{Ref, Schedule};

/// A payload label: systems under it take `&Click` first.
pub struct Click(pub u32);
impl Schedule for Click {
    type In = Ref<Self>;
}
