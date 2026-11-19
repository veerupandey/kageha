export interface KagehaDesktop {
  embedded: true;
  setGuestBounds: (bounds: { x: number; y: number; width: number; height: number }) => void;
  setGuestInteractive: (interactive: boolean) => void;
}

declare global {
  interface Window {
    kageha?: KagehaDesktop;
  }
}

export {};
