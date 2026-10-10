import test from "node:test";
import assert from "node:assert/strict";
import { productVersions, releaseLabel } from "../../scripts/release.mjs";

const versions = [
  ["frontend", "1.0.0"],
  ["desktop", "1.0.0"],
  ["CLI", "1.0.0"],
];

test("product version checks preserve independent DSP and voice library versions", () => {
  const cargo = {
    workspace_members: ["gui", "cli", "dsp", "voice"],
    packages: [
      { id: "gui", name: "jr-receiver-desktop", version: "1.0.0" },
      { id: "cli", name: "jr-receiver", version: "1.0.0" },
      { id: "dsp", name: "jr_conventional_dsp", version: "1.4.0" },
      { id: "voice", name: "jr_conventional_voice", version: "1.4.0" },
    ],
  };
  assert.equal(releaseLabel(productVersions(cargo), {}), "v1.0.0-dev-local");
  cargo.workspace_members = ["gui", "dsp", "voice"];
  assert.throws(
    () => productVersions(cargo),
    /Missing product package: jr-receiver/,
  );
});

test("release tags must match every product manifest", () => {
  assert.equal(
    releaseLabel(versions, {
      GITHUB_REF_TYPE: "tag",
      GITHUB_REF_NAME: "v1.0.0",
    }),
    "v1.0.0",
  );
  for (const tag of ["v2.0.0", "v1.0.0/../../other", "vlatest"])
    assert.throws(
      () =>
        releaseLabel(versions, {
          GITHUB_REF_TYPE: "tag",
          GITHUB_REF_NAME: tag,
        }),
      /Release tag/,
    );
  assert.throws(
    () => releaseLabel([...versions, ["stale crate", "0.9.0"]], {}),
    /stale crate/,
  );
});

test("manual branch build names use a commit, never the branch name", () => {
  assert.equal(
    releaseLabel(versions, {
      GITHUB_REF_TYPE: "branch",
      GITHUB_REF_NAME: "feature/$(command)",
      GITHUB_SHA: "a".repeat(40),
    }),
    "v1.0.0-dev-aaaaaaaa",
  );
  assert.equal(releaseLabel(versions, {}), "v1.0.0-dev-local");
  assert.throws(
    () => releaseLabel(versions, { GITHUB_SHA: "../invalid" }),
    /Invalid GITHUB_SHA/,
  );
  assert.throws(
    () => releaseLabel([["frontend", "../invalid"]], {}),
    /semantic version/,
  );
});
