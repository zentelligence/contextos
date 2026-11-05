use super::*;

#[test]
fn a_page_file_is_a_base_a_canvas_or_a_diagram_and_nothing_else() {
    assert_eq!(PageFile::try_from("memory/tasks.base"), Ok(PageFile::Base));
    assert_eq!(PageFile::try_from("boards/plan.canvas"), Ok(PageFile::Canvas));
    assert_eq!(PageFile::try_from("flows/a.mermaid"), Ok(PageFile::Diagram));
    for other in ["notes/a.md", "img/p.png", "doc.pdf", "plain"] {
        assert_eq!(PageFile::try_from(other), Err(NotAPageFile), "{other}");
    }
}
