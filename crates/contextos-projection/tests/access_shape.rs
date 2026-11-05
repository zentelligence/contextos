//! `access` is interim: its successor names tool patterns beside `all` and `read-only`. A caller outside this crate must
//! therefore already handle values it does not know today, so adding the successor breaks nobody.

use contextos_projection::{Access, AgentDefinition, ProjectionError, Slug};

fn access_of(frontmatter: &str) -> Result<&'static str, ProjectionError> {
    let source = format!("---\ndescription: d\n{frontmatter}\n---\nbody");
    let definition = AgentDefinition::try_from((&Slug::try_from("reviewer")?, source.as_bytes()))?;
    // Outside the defining crate this match is only exhaustive with a wildcard arm, which is the point of the test.
    Ok(match definition.access() {
        Access::All => "all",
        Access::ReadOnly => "read-only",
        _ => "something else",
    })
}

#[test]
fn a_caller_outside_the_crate_reads_the_current_access_values_and_must_allow_for_others() -> Result<(), ProjectionError>
{
    assert_eq!(access_of("tier: fast")?, "all");
    assert_eq!(access_of("tier: fast\naccess: all")?, "all");
    assert_eq!(access_of("tier: fast\naccess: read-only")?, "read-only");
    Ok(())
}
