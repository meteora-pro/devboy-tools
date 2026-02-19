/**
 * Returns the absolute path to the devboy binary.
 *
 * Resolution order:
 * 1. `DEVBOY_BINARY_PATH` environment variable
 * 2. Platform-specific npm package (`@devboy-tools/{platform}-{arch}`)
 *
 * @throws {Error} If the binary is not found at any location
 */
export declare function getBinaryPath(): string;

/** Binary name: `"devboy"` */
export declare const name: string;

/** Package version (e.g. `"0.3.0"`) */
export declare const version: string;
