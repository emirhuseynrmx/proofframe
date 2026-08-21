from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_privileged_crate_publish_uses_the_verified_source_artifact():
    workflow = (ROOT / ".github" / "workflows" / "publish.yml").read_text(
        encoding="utf-8"
    )
    publish_crate = workflow.split("\n  publish-crate:\n", maxsplit=1)[1]

    assert "actions/checkout@" not in publish_crate
    assert "name: source-zip" in publish_crate
    assert "working-directory: ${{ steps.crate-source.outputs.root }}" in publish_crate
