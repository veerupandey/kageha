import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Icon } from "../../../lib/icons";

export type ImageZoomMode = "fit-screen" | "fit-width" | "actual" | "custom";

export interface ImageViewerProps {
  url: string;
  name: string;
  children?: ReactNode;
  onTouchStart?: (e: React.TouchEvent) => void;
  onTouchEnd?: (e: React.TouchEvent) => void;
  onDimensionsLoaded?: (dims: { width: number; height: number }) => void;
}

export function ImageViewer({
  url,
  name,
  children,
  onTouchStart,
  onTouchEnd,
  onDimensionsLoaded,
}: ImageViewerProps) {
  const [zoomMode, setZoomMode] = useState<ImageZoomMode>("fit-screen");
  const [zoomLevel, setZoomLevel] = useState<number>(1);
  const [naturalSize, setNaturalSize] = useState<{ width: number; height: number } | null>(null);
  const [isPanning, setIsPanning] = useState(false);
  const [panStart, setPanStart] = useState<{ x: number; y: number }>({ x: 0, y: 0 });
  const [scrollStart, setScrollStart] = useState<{ left: number; top: number }>({ left: 0, top: 0 });

  const containerRef = useRef<HTMLDivElement | null>(null);
  const imgRef = useRef<HTMLImageElement | null>(null);

  // Reset state when url changes
  useEffect(() => {
    setZoomMode("fit-screen");
    setZoomLevel(1);
    setNaturalSize(null);
  }, [url]);

  const handleImageLoad = (e: React.SyntheticEvent<HTMLImageElement>) => {
    const img = e.currentTarget;
    const dims = { width: img.naturalWidth, height: img.naturalHeight };
    setNaturalSize(dims);
    onDimensionsLoaded?.(dims);

    // If image is very tall (height > 1.4 * width), auto-select fit-width so it's readable
    if (dims.height > dims.width * 1.4) {
      setZoomMode("fit-width");
    }
  };

  const handleZoomIn = useCallback(() => {
    setZoomMode("custom");
    setZoomLevel((z) => Math.min(5, Math.round((z + 0.25) * 100) / 100));
  }, []);

  const handleZoomOut = useCallback(() => {
    setZoomMode("custom");
    setZoomLevel((z) => Math.max(0.2, Math.round((z - 0.25) * 100) / 100));
  }, []);

  const handleFitScreen = useCallback(() => {
    setZoomMode("fit-screen");
    setZoomLevel(1);
  }, []);

  const handleFitWidth = useCallback(() => {
    setZoomMode("fit-width");
    setZoomLevel(1);
  }, []);

  const handleActualSize = useCallback(() => {
    setZoomMode("actual");
    setZoomLevel(1);
  }, []);

  const handleReset = useCallback(() => {
    if (naturalSize && naturalSize.height > naturalSize.width * 1.4) {
      setZoomMode("fit-width");
    } else {
      setZoomMode("fit-screen");
    }
    setZoomLevel(1);
  }, [naturalSize]);

  // Double click toggles between fit-screen and actual size
  const handleDoubleClick = () => {
    if (zoomMode === "fit-screen") {
      setZoomMode("actual");
      setZoomLevel(1);
    } else {
      setZoomMode("fit-screen");
      setZoomLevel(1);
    }
  };

  // Wheel zoom with Ctrl/Cmd or trackpad pinch
  const handleWheel = (e: React.WheelEvent) => {
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      const delta = e.deltaY;
      const factor = delta > 0 ? 0.9 : 1.1;
      setZoomMode("custom");
      setZoomLevel((z) => Math.min(5, Math.max(0.2, Math.round(z * factor * 100) / 100)));
    }
  };

  // Drag to pan when zoomed
  const handleMouseDown = (e: React.MouseEvent) => {
    if (zoomMode === "fit-screen") return;
    const container = containerRef.current;
    if (!container) return;
    setIsPanning(true);
    setPanStart({ x: e.clientX, y: e.clientY });
    setScrollStart({ left: container.scrollLeft, top: container.scrollTop });
  };

  const handleMouseMove = (e: React.MouseEvent) => {
    if (!isPanning) return;
    const container = containerRef.current;
    if (!container) return;
    const dx = e.clientX - panStart.x;
    const dy = e.clientY - panStart.y;
    container.scrollLeft = scrollStart.left - dx;
    container.scrollTop = scrollStart.top - dy;
  };

  const handleMouseUp = () => {
    setIsPanning(false);
  };

  // Keyboard zoom controls
  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
      if (e.key === "+" || e.key === "=") {
        e.preventDefault();
        handleZoomIn();
      } else if (e.key === "-" || e.key === "_") {
        e.preventDefault();
        handleZoomOut();
      } else if (e.key === "0") {
        e.preventDefault();
        handleActualSize();
      } else if (e.key === "w" || e.key === "W") {
        e.preventDefault();
        handleFitWidth();
      } else if (e.key === "f" || e.key === "F") {
        e.preventDefault();
        handleFitScreen();
      }
    };
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [handleZoomIn, handleZoomOut, handleActualSize, handleFitWidth, handleFitScreen]);

  // Determine image sizing styles based on zoomMode
  let imgStyle: React.CSSProperties = {};
  let containerScrollClass = "overflow-hidden";

  if (zoomMode === "fit-screen") {
    imgStyle = {
      maxWidth: "100%",
      maxHeight: "100%",
      objectFit: "contain",
      width: "auto",
      height: "auto",
      cursor: "zoom-in",
    };
    containerScrollClass = "overflow-hidden";
  } else if (zoomMode === "fit-width") {
    imgStyle = {
      width: "100%",
      maxWidth: "100%",
      height: "auto",
      objectFit: "contain",
      cursor: isPanning ? "grabbing" : "grab",
    };
    containerScrollClass = "overflow-y-auto overflow-x-hidden";
  } else if (zoomMode === "actual") {
    imgStyle = {
      width: naturalSize ? `${naturalSize.width}px` : "auto",
      height: naturalSize ? `${naturalSize.height}px` : "auto",
      maxWidth: "none",
      maxHeight: "none",
      cursor: isPanning ? "grabbing" : "grab",
    };
    containerScrollClass = "overflow-auto";
  } else if (zoomMode === "custom") {
    const baseW = naturalSize ? naturalSize.width : 1000;
    const baseH = naturalSize ? naturalSize.height : 800;
    imgStyle = {
      width: `${Math.round(baseW * zoomLevel)}px`,
      height: `${Math.round(baseH * zoomLevel)}px`,
      maxWidth: "none",
      maxHeight: "none",
      cursor: isPanning ? "grabbing" : "grab",
    };
    containerScrollClass = "overflow-auto";
  }

  const zoomPercentDisplay =
    zoomMode === "actual"
      ? "100%"
      : zoomMode === "fit-screen"
        ? "Fit"
        : zoomMode === "fit-width"
          ? "Width"
          : `${Math.round(zoomLevel * 100)}%`;

  return (
    <div
      className="relative flex h-full w-full min-h-0 flex-1 flex-col bg-[#07090e]"
      onTouchStart={onTouchStart}
      onTouchEnd={onTouchEnd}
    >
      {/* Scrollable / pannable canvas */}
      <div
        ref={containerRef}
        className={`relative flex h-full w-full min-h-0 flex-1 items-center justify-center p-4 select-none ${containerScrollClass}`}
        onWheel={handleWheel}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseUp}
      >
        <img
          ref={imgRef}
          src={url}
          alt={name}
          decoding="async"
          draggable={false}
          style={imgStyle}
          className="transition-transform duration-75 select-none drop-shadow-md"
          onLoad={handleImageLoad}
          onDoubleClick={handleDoubleClick}
        />
        {children}
      </div>

      {/* Floating Zoom / Mode Bar at bottom center */}
      <div className="pointer-events-none absolute inset-x-0 bottom-4 z-20 flex justify-center">
        <div className="pointer-events-auto inline-flex items-center gap-1 rounded-full border border-white/15 bg-black/75 px-3 py-1.5 shadow-2xl backdrop-blur-md">
          <button
            type="button"
            className="flex h-7 w-7 items-center justify-center rounded-full text-white/80 hover:bg-white/20 hover:text-white transition-colors"
            title="Zoom out (-)"
            onClick={handleZoomOut}
            aria-label="Zoom out"
          >
            <Icon.ZoomOut size={15} />
          </button>

          <button
            type="button"
            className="px-2 py-0.5 text-xs font-semibold text-white/90 hover:text-white hover:bg-white/10 rounded transition-colors"
            title="Reset to 100% (0)"
            onClick={handleActualSize}
            aria-label="Reset zoom to 100%"
          >
            {zoomPercentDisplay}
          </button>

          <button
            type="button"
            className="flex h-7 w-7 items-center justify-center rounded-full text-white/80 hover:bg-white/20 hover:text-white transition-colors"
            title="Zoom in (+)"
            onClick={handleZoomIn}
            aria-label="Zoom in"
          >
            <Icon.ZoomIn size={15} />
          </button>

          <span className="h-4 w-px bg-white/20 mx-1" />

          <button
            type="button"
            className={`px-2.5 py-1 text-[0.72rem] font-medium rounded-full transition-colors ${
              zoomMode === "fit-screen"
                ? "bg-white/25 text-white font-semibold"
                : "text-white/70 hover:bg-white/15 hover:text-white"
            }`}
            title="Fit image to screen (F)"
            onClick={handleFitScreen}
          >
            Fit Screen
          </button>

          <button
            type="button"
            className={`px-2.5 py-1 text-[0.72rem] font-medium rounded-full transition-colors ${
              zoomMode === "fit-width"
                ? "bg-white/25 text-white font-semibold"
                : "text-white/70 hover:bg-white/15 hover:text-white"
            }`}
            title="Fit to width & scroll vertically (W)"
            onClick={handleFitWidth}
          >
            Fit Width
          </button>

          <button
            type="button"
            className={`px-2.5 py-1 text-[0.72rem] font-medium rounded-full transition-colors ${
              zoomMode === "actual"
                ? "bg-white/25 text-white font-semibold"
                : "text-white/70 hover:bg-white/15 hover:text-white"
            }`}
            title="100% Actual size (0)"
            onClick={handleActualSize}
          >
            100%
          </button>

          <button
            type="button"
            className="flex h-7 w-7 items-center justify-center rounded-full text-white/80 hover:bg-white/20 hover:text-white transition-colors ml-0.5"
            title="Reset"
            onClick={handleReset}
            aria-label="Reset view"
          >
            <Icon.Reset size={14} />
          </button>
        </div>
      </div>
    </div>
  );
}
