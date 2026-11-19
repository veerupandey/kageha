import { useCallback, useEffect, useState } from "react";

export function useDocText(url: string, initialText = "", fallbackError = "Could not load file.") {
  const [text, setText] = useState(initialText);
  const [loading, setLoading] = useState(!initialText);

  useEffect(() => {
    if (initialText) {
      setText(initialText);
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    fetch(url)
      .then((r) => {
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        return r.text();
      })
      .then((body) => {
        if (!cancelled) {
          setText(body);
          setLoading(false);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setText(fallbackError);
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [url, initialText, fallbackError]);

  return { text, loading, setText };
}

export function useCopy() {
  const [copied, setCopied] = useState(false);
  const copy = useCallback((val: string) => {
    void navigator.clipboard?.writeText(val).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }, []);
  return { copied, copy };
}
