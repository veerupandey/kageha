const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("kageha", {
  embedded: true,
  setGuestBounds: (bounds) => {
    ipcRenderer.send("kageha:guest-bounds", bounds);
  },
  setGuestInteractive: (interactive) => {
    ipcRenderer.send("kageha:guest-interactive", Boolean(interactive));
  },
});
