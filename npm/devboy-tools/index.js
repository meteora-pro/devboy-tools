"use strict";

const fs = require("fs");
const path = require("path");

const pkg = JSON.parse(
  fs.readFileSync(path.join(__dirname, "package.json"), "utf8"),
);

/** Package name */
exports.name = "devboy";

/** Package version */
exports.version = pkg.version;

/**
 * Returns the path to the devboy binary.
 *
 * Resolution order:
 * 1. DEVBOY_BINARY_PATH environment variable
 * 2. Platform-specific npm package (@devboy-tools/{platform}-{arch})
 *
 * @returns {string} Absolute path to the devboy binary
 * @throws {Error} If binary is not found
 */
exports.getBinaryPath = function getBinaryPath() {
  // 1. Environment variable override
  const envPath = process.env.DEVBOY_BINARY_PATH;
  if (envPath) {
    if (!fs.existsSync(envPath)) {
      throw new Error(
        `DEVBOY_BINARY_PATH is set to "${envPath}" but the file does not exist.`,
      );
    }
    return path.resolve(envPath);
  }

  // 2. Platform-specific package
  const platformPkg = `@devboy-tools/${process.platform}-${process.arch}`;
  const ext = process.platform === "win32" ? ".exe" : "";
  const binaryName = `devboy${ext}`;

  try {
    const pkgJsonPath = require.resolve(`${platformPkg}/package.json`);
    const binaryPath = path.join(path.dirname(pkgJsonPath), "bin", binaryName);
    if (fs.existsSync(binaryPath)) {
      return binaryPath;
    }
  } catch {
    // Package not installed
  }

  throw new Error(
    `devboy binary not found. No package ${platformPkg} installed.\n` +
      "Your platform might not be supported. " +
      "Set DEVBOY_BINARY_PATH to point to a devboy binary, or install from source:\n" +
      "  cargo install --git https://github.com/meteora-pro/devboy-tools.git",
  );
};
