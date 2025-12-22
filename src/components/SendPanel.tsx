import { useState, KeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";

interface SendPanelProps {
    connected: boolean;
    onSend?: (bytes: number) => void;
}

type LineEnding = "NONE" | "CR" | "LF" | "CRLF";
type InputMode = "TEXT" | "HEX";

export default function SendPanel({ connected, onSend }: SendPanelProps) {
    const [inputMode, setInputMode] = useState<InputMode>("TEXT");
    const [inputText, setInputText] = useState("");
    const [lineEnding, setLineEnding] = useState<LineEnding>("LF");
    const [sendOnEnter, setSendOnEnter] = useState(false);

    // History
    const [history, setHistory] = useState<string[]>([]);
    const [historyIndex, setHistoryIndex] = useState(-1);

    const handleSend = async () => {
        if (!connected || !inputText) return;

        let dataToSend: number[] = [];

        try {
            if (inputMode === "HEX") {
                // Parse hex string (ignore spaces)
                const cleanHex = inputText.replace(/\s+/g, "");
                if (cleanHex.length % 2 !== 0) {
                    alert("Invalid Hex: Odd number of characters");
                    return;
                }
                for (let i = 0; i < cleanHex.length; i += 2) {
                    const byte = parseInt(cleanHex.substring(i, i + 2), 16);
                    if (isNaN(byte)) throw new Error("Invalid hex character");
                    dataToSend.push(byte);
                }
            } else {
                // Convert string to bytes (UTF-8)
                const encoder = new TextEncoder();
                dataToSend = Array.from(encoder.encode(inputText));

                // Append line ending
                switch (lineEnding) {
                    case "CR": dataToSend.push(0x0D); break;
                    case "LF": dataToSend.push(0x0A); break;
                    case "CRLF": dataToSend.push(0x0D, 0x0A); break;
                }
            }

            await invoke("write_data", { data: dataToSend });

            // Update history
            setHistory(prev => {
                // If expected input is same as the most recent one, don't add it
                if (prev.length > 0 && prev[0] === inputText) {
                    return prev;
                }
                const newHistory = [inputText, ...prev].slice(0, 20); // Keep last 20
                return newHistory;
            });
            setHistoryIndex(-1);

            // Clear input if needed (optional: keep for repeat send?)
            // For now, let's clear it to emulate standard terminals, 
            // or maybe keep it if user wants to spam. 
            // Let's clear it for now.
            setInputText("");

            if (onSend) onSend(dataToSend.length);

        } catch (e) {
            console.error(e);
            alert("Failed to send: " + e);
        }
    };

    const handleKeyDown = (e: KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) => {
        if (e.key === "Enter") {
            if (sendOnEnter && !e.shiftKey) {
                e.preventDefault();
                handleSend();
            }
        } else if (e.key === "ArrowUp") {
            e.preventDefault();
            if (history.length > 0) {
                const nextIndex = Math.min(historyIndex + 1, history.length - 1);
                setHistoryIndex(nextIndex);
                setInputText(history[nextIndex]);
            }
        } else if (e.key === "ArrowDown") {
            e.preventDefault();
            if (historyIndex > 0) {
                const nextIndex = historyIndex - 1;
                setHistoryIndex(nextIndex);
                setInputText(history[nextIndex]);
            } else if (historyIndex === 0) {
                setHistoryIndex(-1);
                setInputText("");
            }
        }
    };

    return (
        <div className="send-panel p-2 border rounded bg-gray-50 flex flex-col gap-2">
            <div className="flex gap-2 items-center mb-1">
                <label className="text-sm font-bold flex items-center gap-1">
                    <input
                        type="radio"
                        checked={inputMode === "TEXT"}
                        onChange={() => setInputMode("TEXT")}
                    /> Text
                </label>
                <label className="text-sm font-bold flex items-center gap-1">
                    <input
                        type="radio"
                        checked={inputMode === "HEX"}
                        onChange={() => setInputMode("HEX")}
                    /> Hex
                </label>

                <div className="flex-1"></div>

                <select
                    value={lineEnding}
                    onChange={(e) => setLineEnding(e.target.value as LineEnding)}
                    className="text-sm border rounded p-1"
                    disabled={inputMode === "HEX"}
                >
                    <option value="NONE">No Line Ending</option>
                    <option value="LF">LF (\n)</option>
                    <option value="CR">CR (\r)</option>
                    <option value="CRLF">CRLF (\r\n)</option>
                </select>

                <label className="text-sm flex items-center gap-1">
                    <input
                        type="checkbox"
                        checked={sendOnEnter}
                        onChange={(e) => setSendOnEnter(e.target.checked)}
                    /> Send on Enter
                </label>
            </div>

            <div className="flex gap-2">
                <input
                    type="text"
                    className="flex-1 border rounded p-2 font-mono"
                    placeholder={inputMode === "HEX" ? "e.g. 48 65 6C 6C 6F" : "Type message..."}
                    value={inputText}
                    onChange={(e) => setInputText(e.target.value)}
                    onKeyDown={handleKeyDown}
                    disabled={!connected}
                />
                <button
                    onClick={handleSend}
                    disabled={!connected || !inputText}
                    className="px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50"
                >
                    Send
                </button>
            </div>
        </div>
    );
}
