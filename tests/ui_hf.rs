//! M8.2 reducer + small-window law for the real HF widget.
use lsmtop::hf::TreeFile;
use lsmtop::ui::hf::{HfKey, HfOut, HfState, HfWidget};
use ratatui::{backend::TestBackend, widgets::Widget, Terminal};

fn render<W: Widget>(w: W, width: u16, height: u16) -> Vec<String> {
    let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
    t.draw(move |f| w.render(f.area(), f.buffer_mut())).unwrap();
    let b = t.backend().buffer().clone();
    (0..b.area().height)
        .map(|y| {
            (0..b.area().width)
                .map(|x| {
                    b.cell((x, y))
                        .map(|c| c.symbol().chars().next().unwrap_or(' '))
                        .unwrap_or(' ')
                })
                .collect::<String>()
        })
        .collect()
}

#[test]
fn enter_parses_ref_or_barks() {
    let mut st = HfState::new(" hf: x ".into());
    for c in "https://huggingface.co/org/repo".chars() {
        st.apply(HfKey::Char(c));
    }
    assert_eq!(st.apply(HfKey::Enter), HfOut::ListRepo("org/repo".into()));
    assert!(st.input.is_empty(), "input cleared for next entry");
    // explicit file link downloads right away
    for c in "org/repo/resolve/main/m.gguf".chars() {
        st.apply(HfKey::Char(c));
    }
    assert_eq!(
        st.apply(HfKey::Enter),
        HfOut::Download("org/repo".into(), vec!["m.gguf".into()])
    );
    // garbage barks into the log, never panics
    let mut st2 = HfState::new("".into());
    for c in "hello world".chars() {
        st2.apply(HfKey::Char(c));
    }
    assert_eq!(st2.apply(HfKey::Enter), HfOut::Nothing);
    assert!(st2.log.last().unwrap().contains("not an HF link"));
}

#[test]
fn listing_selection_numbers_names_all() {
    let mut st = HfState::new("".into());
    st.set_listing(
        "org/repo",
        &[
            TreeFile { path: "a/q4.gguf".into(), size: 4_000_000_000 },
            TreeFile { path: "b/q8.gguf".into(), size: 8_000_000_000 },
        ],
    );
    assert!(st.log.iter().any(|l| l.contains("Found 2 GGUF")));
    assert!(st.log.iter().any(|l| l.contains("4.00 GB")), "sizes humanized");
    // number selection, 'all', and literal names through Download
    st.input = "1".into();
    assert_eq!(
        st.apply(HfKey::Download),
        HfOut::Download("org/repo".into(), vec!["a/q4.gguf".into()])
    );
    st.input = "all".into();
    assert_eq!(st.apply(HfKey::Download), HfOut::Download("org/repo".into(), vec!["a/q4.gguf".into(), "b/q8.gguf".into()]));
    st.input = "weird/file.gguf".into();
    assert_eq!(
        st.apply(HfKey::Download),
        HfOut::Download("org/repo".into(), vec!["weird/file.gguf".into()]),
        "literal names pass through"
    );
}

#[test]
fn hf_screen_law_at_all_sizes() {
    for (w, h) in [(80u16, 24u16), (70, 18), (60, 14)] {
        let mut st = HfState::new(" hf: /usr/bin/hf · token: used ".into());
        for i in 0..50 {
            st.push_log(format!("downloading chunk {i}"));
        }
        st.input = "org/repo".into();
        let lines = render(HfWidget(&st), w, h);
        let last = lines.last().unwrap();
        assert!(last.contains("esc Back"), "{w}x{h}: pinned footer lost: {last:?}");
        assert!(lines[0].contains("hf:"), "{w}x{h}: env line lost");
        let joined = lines.join("\n");
        assert!(joined.contains("org/repo"), "{w}x{h}: input lost");
        assert!(joined.contains("chunk 49"), "{w}x{h}: live tail lost");
    }
}

#[test]
fn token_value_never_renders_on_screen() {
    // env_line shows only 'used'/'none' — assert nothing token-like appears
    let st = HfState::new(" hf: /usr/bin/hf · token: used ".into());
    let joined = render(HfWidget(&st), 80, 24).join("\n");
    assert!(joined.contains("token: used"));
    assert!(!joined.contains("hf_"), "no raw token on screen");
}
