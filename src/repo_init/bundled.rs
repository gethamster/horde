macro_rules! skill {
    ($path:literal) => {
        (
            $path,
            include_bytes!(concat!("../../skills/", $path)).as_slice(),
        )
    };
}

pub(super) fn files() -> Vec<(&'static str, &'static [u8])> {
    vec![
        skill!("horde/SKILL.md"),
        skill!("horde/references/configuration.md"),
        skill!("horde/references/delegation.md"),
        skill!("horde/references/delivery.md"),
        skill!("horde/references/environments.md"),
        skill!("horde/references/fleet.md"),
        skill!("horde/references/operations.md"),
        skill!("horde/references/setup.md"),
        skill!("horde/references/tasks.md"),
        skill!("horde-setup/SKILL.md"),
        skill!("horde-delegation/SKILL.md"),
        skill!("horde-discovery/SKILL.md"),
        skill!("horde-planning/SKILL.md"),
        skill!("horde-planning/horde.toml"),
        skill!("horde-model-selection/SKILL.md"),
        skill!("horde-model-selection/horde.toml"),
        skill!("horde-review/SKILL.md"),
        skill!("horde-worker/SKILL.md"),
        skill!("horde-worker/references/operations.md"),
        skill!("horde-templates/SKILL.md"),
        skill!("horde-templates/references/examples.md"),
        skill!("horde-sdlc/SKILL.md"),
        skill!("horde-sdlc/references/artifacts.md"),
        skill!("horde-sdlc/references/execution.md"),
    ]
}
