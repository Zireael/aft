/// <reference path="../bun-test.d.ts" />

// npm on Windows does not symlink a package's bin into the global prefix. It
// writes small launcher scripts there instead (`opencode.cmd` for cmd.exe, an
// extensionless `opencode` for POSIX shells, `opencode.ps1` for PowerShell)
// and keeps the package under `<prefix>/node_modules`. These fixtures rebuild
// that layout with the exact launcher text npm's cmd-shim writes, so the
// Windows behaviour is exercised on every platform through the `platform`
// injection rather than only on a Windows runner.

import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { detectOpenCodeHostGeneration, probeOpenCodeV1Version } from "../setup/host-generation.js";
import { MODERN_V1_VERSION } from "../setup/opencode-config.js";

const CMD_SHIM_HEAD =
  "@ECHO off\r\n" +
  "GOTO start\r\n" +
  ":find_dp0\r\n" +
  "SET dp0=%~dp0\r\n" +
  "EXIT /b\r\n" +
  ":start\r\n" +
  "SETLOCAL\r\n" +
  "CALL :find_dp0\r\n";

const SH_SHIM_HEAD =
  "#!/bin/sh\n" +
  `basedir=$(dirname "$(echo "$0" | sed -e 's,\\\\,/,g')")\n` +
  "\n" +
  "case `uname` in\n" +
  "    *CYGWIN*|*MINGW*|*MSYS*)\n" +
  "        if command -v cygpath > /dev/null 2>&1; then\n" +
  '            basedir=`cygpath -w "$basedir"`\n' +
  "        fi\n" +
  "    ;;\n" +
  "esac\n" +
  "\n";

/** cmd-shim's `.cmd` for a target with no shebang, such as a native `.exe`. */
function nativeCmdShim(target: string): string {
  return `${CMD_SHIM_HEAD}"%dp0%\\${target.split("/").join("\\")}"   %*\r\n`;
}

/** cmd-shim's extensionless sh launcher for a target with no shebang. */
function nativeShShim(target: string): string {
  return `${SH_SHIM_HEAD}exec "$basedir/${target}"   "$@"\n`;
}

/** cmd-shim's `.cmd` for a `#!/usr/bin/env node` script, as OpenCode 1 ships. */
function nodeCmdShim(target: string): string {
  return (
    CMD_SHIM_HEAD +
    "\r\n" +
    'IF EXIST "%dp0%\\node.exe" (\r\n' +
    '  SET "_prog=%dp0%\\node.exe"\r\n' +
    ") ELSE (\r\n" +
    '  SET "_prog=node"\r\n' +
    "  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n" +
    ")\r\n" +
    "\r\n" +
    `endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\\${target
      .split("/")
      .join("\\")}" %*\r\n`
  );
}

interface NpmPrefix {
  prefix: string;
  binary: string;
}

/**
 * A global npm prefix holding `@opencode/cli@2.0.22`, whose package.json maps
 * both `opencode` and `opencode2` onto `bin/opencode.exe`. `shells` picks which
 * launchers npm left beside each command name.
 */
function npmPrefixWithOpenCode2(label: string, shells: Array<"cmd" | "sh">): NpmPrefix {
  const prefix = mkdtempSync(join(tmpdir(), label));
  const packageRoot = join(prefix, "node_modules", "@opencode", "cli");
  mkdirSync(join(packageRoot, "bin"), { recursive: true });
  writeFileSync(
    join(packageRoot, "package.json"),
    JSON.stringify({
      name: "@opencode/cli",
      version: "2.0.22",
      bin: { opencode: "bin/opencode.exe", opencode2: "bin/opencode.exe" },
    }),
  );
  const binary = join(packageRoot, "bin", "opencode.exe");
  writeFileSync(binary, "MZ placeholder for the native OpenCode 2 binary\n");
  const target = "node_modules/@opencode/cli/bin/opencode.exe";
  for (const name of ["opencode", "opencode2"]) {
    if (shells.includes("cmd")) writeFileSync(join(prefix, `${name}.cmd`), nativeCmdShim(target));
    if (shells.includes("sh")) writeFileSync(join(prefix, name), nativeShShim(target));
  }
  return { prefix, binary };
}

describe("OpenCode host detection through npm's Windows launchers", () => {
  // `npm i -g @opencode/cli` on Windows: PATH holds the npm prefix, and
  // nothing in it is a symlink to the package.
  for (const shells of [["cmd"], ["cmd", "sh"]] as Array<Array<"cmd" | "sh">>) {
    test(`reads @opencode/cli behind ${shells.join(" + ")} launchers as V2 from package metadata`, () => {
      const { prefix } = npmPrefixWithOpenCode2("aft-cli-npm-shim-v2-", shells);
      const probed: string[] = [];

      const result = detectOpenCodeHostGeneration({
        path: prefix,
        platform: "win32",
        probeV1Version: (executable) => {
          probed.push(executable);
          return null;
        },
      });

      expect(result.status).toBe("v2");
      expect(result.generations).toEqual(["v2"]);
      expect(result.evidence).toHaveLength(1);
      expect(result.evidence[0]?.version).toBe("2.0.22");
      // Package metadata settled it, so nothing was run.
      expect(probed).toEqual([]);
    });
  }

  // `opencode.cmd` and `opencode2.cmd` are two files, so their realpaths
  // differ, but both launch the one binary the package maps both names onto.
  test("treats opencode.cmd and opencode2.cmd for one package as one installation", () => {
    const { prefix } = npmPrefixWithOpenCode2("aft-cli-npm-shim-shared-", ["cmd"]);

    const result = detectOpenCodeHostGeneration({
      platform: "win32",
      findExecutable: (name) => join(prefix, `${name}.cmd`),
      probeV1Version: () => null,
    });

    expect(result.status).toBe("v2");
    expect(result.evidence).toHaveLength(1);
    expect(result.evidence[0]?.executable).toBe(join(prefix, "opencode.cmd"));
  });

  // OpenCode 1's npm package ships a node script, so its launcher runs node
  // with the script as an argument. Its metadata must still be found, and must
  // still say V1.
  test("reads an npm-installed OpenCode 1 behind its node launcher as V1", () => {
    const prefix = mkdtempSync(join(tmpdir(), "aft-cli-npm-shim-v1-"));
    const packageRoot = join(prefix, "node_modules", "opencode-ai");
    mkdirSync(join(packageRoot, "bin"), { recursive: true });
    writeFileSync(
      join(packageRoot, "package.json"),
      JSON.stringify({ name: "opencode-ai", version: MODERN_V1_VERSION }),
    );
    writeFileSync(join(packageRoot, "bin", "opencode"), "#!/usr/bin/env node\n");
    writeFileSync(
      join(prefix, "opencode.cmd"),
      nodeCmdShim("node_modules/opencode-ai/bin/opencode"),
    );
    const probed: string[] = [];

    const result = detectOpenCodeHostGeneration({
      path: prefix,
      platform: "win32",
      probeV1Version: (executable) => {
        probed.push(executable);
        return null;
      },
    });

    expect(result.status).toBe("v1");
    expect(result.evidence[0]?.version).toBe(MODERN_V1_VERSION);
    expect(result.evidence[0]?.modernV1).toBe(true);
    expect(probed).toEqual([]);
  });
});

describe("OpenCode host version probe and batch launchers", () => {
  // Node refuses to start a `.cmd` or `.bat` without a shell (EINVAL since the
  // CVE-2024-27980 fix), so spawning the launcher itself never yields a
  // version and the host was filed as OpenCode 1.
  test("probes the binary a .cmd launcher names, never the .cmd itself", () => {
    const { prefix, binary } = npmPrefixWithOpenCode2("aft-cli-npm-shim-probe-", ["cmd"]);
    const calls: string[] = [];

    const version = probeOpenCodeV1Version(join(prefix, "opencode.cmd"), {
      platform: "win32",
      operatorHome: join(prefix, "operator"),
      tempParent: prefix,
      spawn: (executable) => {
        calls.push(executable);
        return { status: 0, stdout: "opencode v2.0.22\n" };
      },
    });

    expect(version).toBe("2.0.22");
    expect(calls).toEqual([binary]);
  });

  test("returns no version rather than spawning a .cmd it cannot see through", () => {
    const root = mkdtempSync(join(tmpdir(), "aft-cli-opaque-cmd-"));
    const launcher = join(root, "opencode.cmd");
    writeFileSync(launcher, "@echo off\r\nexit /b 0\r\n");
    const calls: string[] = [];

    for (const platform of ["win32", "linux"] as const) {
      const version = probeOpenCodeV1Version(launcher, {
        platform,
        operatorHome: join(root, "operator"),
        tempParent: root,
        spawn: (executable) => {
          calls.push(executable);
          return { status: 0, stdout: "opencode v2.0.22\n" };
        },
      });
      expect(version).toBeNull();
    }

    expect(calls).toEqual([]);
  });
});
