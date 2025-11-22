// src/modules/ui-console.js
const { invoke } = window.__TAURI__.core;
// Note: In Tauri v2, event listener is usually under window.__TAURI__.event
const { listen } = window.__TAURI__.event; 

const MAX_LINES = 500; // Limit to prevent lag
let unlistenFn = null; // To clean up listener when app closes or changes

// DOM Elements
const consoleWindow = document.querySelector(".console-window");
const consoleInput = document.querySelector(".console-input");

export async function setupConsole(state) {
    // Clear fake logs initially
    consoleWindow.innerHTML = "";

    // 1. Listen for Rust events
    if (unlistenFn) {
        unlistenFn();
    }

    unlistenFn = await listen('server-output', (event) => {
        const { id, line } = event.payload;

        // Only show logs for the currently active server
        if (state.activeServerId === id) {
            appendLogLine(line);
        }
    });

    // 2. Handle Command Input
    consoleInput.addEventListener('keydown', async (e) => {
        if (e.key === 'Enter') {
            const cmd = consoleInput.value.trim();
            if (cmd && state.activeServerId) {
                try {
                    // Echo the command to console immediately for UX
                    appendLogLine(`> ${cmd}`, true);
                    
                    // Send to Backend
                    await invoke('send_command', { 
                        id: state.activeServerId, 
                        command: cmd 
                    });
                    
                    consoleInput.value = ""; // Clear input
                } catch (err) {
                    appendLogLine(`[Error sending command]: ${err}`, true);
                }
            }
        }
    });
}

function appendLogLine(text, isUserCommand = false) {
    const div = document.createElement("div");
    div.className = "console-line";
    div.textContent = text; // Use textContent to prevent XSS
    
    if (isUserCommand) {
        div.style.color = "var(--accent)";
        div.style.fontWeight = "bold";
    } else {
        // Simple coloring based on keywords
        if (text.toLowerCase().includes("warn")) div.classList.add("warn");
        if (text.toLowerCase().includes("error") || text.toLowerCase().includes("exception")) div.classList.add("error");
    }

    consoleWindow.appendChild(div);

    // LIMIT LINES (Performance)
    if (consoleWindow.children.length > MAX_LINES) {
        consoleWindow.removeChild(consoleWindow.firstChild);
    }

    // Auto-scroll to bottom
    consoleWindow.scrollTop = consoleWindow.scrollHeight;
}

// Function to clear console when switching servers
export function clearConsole() {
    consoleWindow.innerHTML = "";
}