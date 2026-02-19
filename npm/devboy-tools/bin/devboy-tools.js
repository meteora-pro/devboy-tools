#!/usr/bin/env node

"use strict";

const { spawn } = require("child_process");
const { getBinaryPath } = require("../index");

let binaryPath;
try {
  binaryPath = getBinaryPath();
} catch (err) {
  console.error(err.message);
  process.exit(1);
}

const child = spawn(binaryPath, process.argv.slice(2), {
  stdio: "inherit",
});

child.on("error", (err) => {
  if (err.code === "ENOENT") {
    console.error(
      `devboy binary not found at: ${binaryPath}\n` +
        "Run 'npm rebuild @devboy-tools/cli' or set DEVBOY_BINARY_PATH.",
    );
  } else {
    console.error(`Failed to start devboy: ${err.message}`);
  }
  process.exit(1);
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});

// Forward signals to child process
for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    if (!child.killed) {
      child.kill(sig);
    }
  });
}
