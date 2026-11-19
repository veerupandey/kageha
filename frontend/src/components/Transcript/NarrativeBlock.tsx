import { memo } from "react";
import { useAppStore } from "../../store";
import { StreamingMarkdown } from "./StreamingMarkdown";

export const NarrativeBlock = memo(function NarrativeBlock({
  id,
  text,
  streaming,
  isUser,
}: {
  id: string;
  text: string;
  streaming?: boolean;
  isUser?: boolean;
}) {
  const sessionId = useAppStore((s) => s.sessionId);
  const openCanvasItem = useAppStore((s) => s.openCanvasItem);

  return (
    <StreamingMarkdown
      id={id}
      text={text}
      streaming={streaming}
      sessionId={sessionId}
      isUser={isUser}
      onArtifactClick={(path, expand) => openCanvasItem(path, { expand })}
    />
  );
});
