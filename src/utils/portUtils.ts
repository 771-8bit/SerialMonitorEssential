/**
 * Device path of a `list_ports` entry: "Pico (COM15)" → "COM15",
 * "USB Serial (/dev/ttyUSB0)" → "/dev/ttyUSB0", "COM3" → "COM3".
 *
 * Same rule as `extract_port_path` in src-tauri/src/serial/mod.rs (the content of the
 * last parentheses, or the whole name). Auto reconnect matches on this path, because the
 * friendly part of the name can change when a device re-enumerates.
 */
export function portPath(portName: string): string {
  const start = portName.lastIndexOf('(');
  const end = portName.lastIndexOf(')');
  return start !== -1 && end !== -1 && start < end ? portName.slice(start + 1, end) : portName;
}
