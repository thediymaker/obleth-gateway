#[derive(Clone, Debug, PartialEq)]
pub enum ModelAction {
    Create,
    Update(String),
}

pub fn plan_model(existing_id: Option<&str>) -> ModelAction {
    match existing_id {
        Some(id) => ModelAction::Update(id.to_string()),
        None => ModelAction::Create,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_action_create_vs_update() {
        assert_eq!(plan_model(None), ModelAction::Create);
        assert_eq!(plan_model(Some("m9")), ModelAction::Update("m9".into()));
    }
}
