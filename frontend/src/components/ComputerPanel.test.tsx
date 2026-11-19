import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ComputerState } from "../api/types";
import { api } from "../api/client";
import { useAppStore } from "../store";
import { ComputerPanel } from "./ComputerPanel";

vi.mock("../api/client", () => ({
  api: vi.fn(),
}));

const computerState: ComputerState = {
  sessionId: "session-1",
  provider: "local_mac",
  available: true,
  connected: true,
  app: "Notes",
  lastAction: "launch",
  agentBound: true,
  lockedBy: "agent",
  canControl: false,
  frameVersion: 4,
  frame: {
    path: "artifacts/computer/live/user_current.png",
    thumb_path: "artifacts/computer/thumbs/user_current.jpg",
    action: "launch",
    width: 900,
    height: 620,
  },
};

describe("ComputerPanel live preview", () => {
  beforeEach(() => {
    vi.mocked(api).mockResolvedValue(computerState);
    useAppStore.setState({
      sessionId: "session-1",
      runStatus: "running",
      computerPanelMode: "expanded",
      messages: [],
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    useAppStore.setState({ sessionId: null, messages: [] });
  });

  it("requires an explicit operator gesture after human intervention", async () => {
    vi.mocked(api).mockResolvedValue({...computerState,runtime:"rust",lockedBy:"user",canControl:true,interruptionReason:"human_input"});
    render(<ComputerPanel />);
    expect(await screen.findByText("Paused because you used the mouse or keyboard.")).toBeInTheDocument();
    expect(vi.mocked(api).mock.calls.some(([path])=>String(path).endsWith("/lease"))).toBe(false);
    fireEvent.click(screen.getByRole("button",{name:"Resume manual control"}));
    await waitFor(()=>expect(vi.mocked(api).mock.calls.some(([path,options])=>String(path).endsWith("/lease") && options?.body === JSON.stringify({action:"take"}))).toBe(true));
  });

  it("shows the latest captured app frame", async () => {
    render(<ComputerPanel />);

    await waitFor(() => {
      expect(screen.getByAltText("Live desktop")).toHaveAttribute(
        "src",
        "/api/sessions/session-1/files/artifacts/computer/live/user_current.png?v=4",
      );
    });
  });

  it("explains an unsupported preview instead of waiting indefinitely", async () => {
    vi.mocked(api).mockResolvedValue({ ...computerState, frame: undefined, previewAvailable: false });
    const { container } = render(<ComputerPanel />);
    expect(await screen.findByText("Live desktop preview is not available")).toBeInTheDocument();
    expect(screen.getByText("Watch the app on your Mac. Agent actions are listed in Activity.")).toBeInTheDocument();
    expect(container.querySelector(".stage-boot-ring")).toBeNull();
    expect(screen.queryByText(/Waiting for .*preview/)).not.toBeInTheDocument();
  });

  it("prefers the current live frame over a transcript thumbnail", async () => {
    useAppStore.setState({
      messages: [
        {
          id: "assistant-1",
          role: "assistant",
          text: "",
          computerFrames: [
            {
              app: "Notes",
              action: "click",
              path: "artifacts/computer/thumbs/action_0001.jpg",
              url: "/api/sessions/session-1/files/artifacts/computer/thumbs/action_0001.jpg?v=1",
            },
          ],
        },
      ],
    });

    render(<ComputerPanel />);

    expect((await screen.findAllByText("Notes · click")).length).toBeGreaterThan(0);
    await waitFor(() => expect(screen.getByAltText("Live desktop")).toHaveAttribute(
      "src",
      "/api/sessions/session-1/files/artifacts/computer/live/user_current.png?v=4",
    ));
  });

  it("shows a running computer tool before its screenshot arrives", async () => {
    useAppStore.setState({
      messages: [
        {
          id: "assistant-1",
          role: "assistant",
          text: "",
          toolCards: [
            {
              id: "tool-1",
              name: "computer_type",
              status: "running",
            },
          ],
        },
      ],
    });

    render(<ComputerPanel />);

    expect((await screen.findAllByText("Notes · type")).length).toBeGreaterThan(0);
  });
  it("falls back to the agent image when the live image fails", async () => {
    useAppStore.setState({ messages: [{ id: "assistant-1", role: "assistant", text: "",
      computerFrames: [{ app: "Notes", url: "/agent.jpg" }] }] });
    render(<ComputerPanel />);
    await waitFor(() => expect(screen.getByAltText("Live desktop")).toHaveAttribute(
      "src", "/api/sessions/session-1/files/artifacts/computer/live/user_current.png?v=4"));
    fireEvent.error(screen.getByAltText("Live desktop"));
    expect(screen.getByAltText("Live desktop")).toHaveAttribute("src", "/agent.jpg");
  });

  it("offers Connect during a run when a disconnected preview fails", async () => {
    vi.mocked(api).mockResolvedValue({ ...computerState, connected: false, agentBound: false,
      frame: undefined, error: "no desktop app is selected" });
    useAppStore.setState({ messages: [{ id: "assistant-1", role: "assistant", text: "",
      computerFrames: [{ app: "Notes", url: "/missing.jpg" }] }] });
    render(<ComputerPanel />);
    await screen.findByText("no desktop app is selected");
    fireEvent.error(screen.getByAltText("Live desktop"));
    expect(screen.queryByAltText("Live desktop")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect" })).toBeEnabled();
  });

  it("shows a disconnected blocker without claiming an app is opening", async () => {
    vi.mocked(api).mockResolvedValue({ sessionId: "session-1", available: true,
      connected: false, error: "no desktop app is selected" });
    render(<ComputerPanel />);
    await screen.findByText("Desktop preview unavailable");
    expect(screen.getByText("no desktop app is selected")).toBeInTheDocument();
    expect(screen.queryByText(/Opening|Connecting/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect" })).toBeEnabled();
  });

  it("distinguishes an agent binding waiting for pixels from no app selected", async () => {
    vi.mocked(api).mockResolvedValue({ ...computerState, connected: false,
      frame: undefined, error: "waiting for Notes window" });
    render(<ComputerPanel />);
    await screen.findByText("Waiting for Notes preview");
    expect(screen.queryByText("Desktop preview unavailable")).not.toBeInTheDocument();
    expect(screen.queryByText(/Opening|Connecting/)).not.toBeInTheDocument();
  });

});

it("renders read-only preview frames without requesting a new model observation", async () => {
  useAppStore.setState({sessionId:"session-1",runStatus:"running",messages:[]});
  vi.mocked(api).mockImplementation(async (url) => url.endsWith("/preview")
    ? {status:"ok",result:{image:{mime_type:"image/jpeg",data:"/9j/test"}}}
    : {...computerState,frame:undefined,previewAvailable:true});
  render(<ComputerPanel />);
  expect(await screen.findByAltText("Live desktop")).toHaveAttribute("src","data:image/jpeg;base64,/9j/test");
  expect(vi.mocked(api).mock.calls.some(([url])=>url.endsWith("/computer/preview"))).toBe(true);
  cleanup();
});

it("does not return user control merely because an agent turn starts", async () => {
  useAppStore.setState({sessionId:"session-1",runStatus:"idle",messages:[]});
  vi.mocked(api).mockResolvedValue({...computerState,lockedBy:"user",previewAvailable:false,frame:undefined});
  render(<ComputerPanel />);
  await screen.findByText("You have control on your Mac. Choose Agent to return control.");
  act(()=>useAppStore.setState({runStatus:"running"}));
  await waitFor(()=>expect(screen.getByRole("tab",{name:"You"})).toHaveAttribute("aria-selected","true"));
  expect(vi.mocked(api).mock.calls.some(([url,init])=>url.endsWith("/lease")&&JSON.stringify(init).includes('return'))).toBe(false);
  cleanup();
});

it("prioritizes takeover during an in-flight preview and ignores its stale response", async () => {
  useAppStore.setState({sessionId:"session-1",runStatus:"running",messages:[]});
  let finishFrame!: (value: unknown) => void;
  const pendingFrame=new Promise(resolve=>{finishFrame=resolve});
  vi.mocked(api).mockImplementation(async (url) => {
    if(url.endsWith("/preview"))return pendingFrame;
    if(url.endsWith("/lease"))return {...computerState,lockedBy:"user",frame:undefined,previewAvailable:false};
    return {...computerState,frame:undefined,previewAvailable:true};
  });
  render(<ComputerPanel />);
  await waitFor(()=>expect(vi.mocked(api).mock.calls.some(([url])=>url.endsWith("/preview"))).toBe(true));
  fireEvent.click(screen.getByRole("tab",{name:"You"}));
  await screen.findByText("You have control on your Mac. Choose Agent to return control.");
  await act(async()=>finishFrame({status:"ok",result:{image:{mime_type:"image/jpeg",data:"/9j/stale"}}}));
  expect(screen.getByRole("tab",{name:"You"})).toHaveAttribute("aria-selected","true");
  expect(screen.queryByAltText("Live desktop")).not.toBeInTheDocument();
  cleanup();
});

it("maps scaled manual clicks to the issued frame and suppresses concurrent input", async () => {
  useAppStore.setState({sessionId:"session-1",runStatus:"idle",messages:[]});
  vi.mocked(api).mockClear();
  let finishInput!: (value: unknown) => void;
  const pendingInput=new Promise(resolve=>{finishInput=resolve});
  vi.mocked(api).mockImplementation(async url=>url.endsWith('/preview')
    ? {status:'ok',frame_id:'issued-frame',result:{width:1000,height:500,image:{mime_type:'image/jpeg',data:'/9j/test'}}}
    : url.endsWith('/action') ? pendingInput : {...computerState,runtime:'rust',lockedBy:'user',canControl:true,previewAvailable:true,frame:undefined});
  render(<ComputerPanel/>);
  const img=await screen.findByAltText('Live desktop');
  vi.spyOn(img,'getBoundingClientRect').mockReturnValue({left:10,top:20,width:500,height:250,right:510,bottom:270,x:10,y:20,toJSON:()=>({})});
  Object.defineProperty(img,'naturalWidth',{value:1000});Object.defineProperty(img,'naturalHeight',{value:500});
  fireEvent.click(img,{clientX:260,clientY:145});fireEvent.click(img,{clientX:270,clientY:155});
  await waitFor(()=>expect(vi.mocked(api).mock.calls.filter(([url])=>url.endsWith('/action'))).toHaveLength(1));
  const request=vi.mocked(api).mock.calls.find(([url])=>url.endsWith('/action'))!;
  expect(JSON.parse(String(request[1]?.body))).toMatchObject({action:'click',x:500,y:250,frame_id:'issued-frame',request_id:expect.any(String)});
  await act(async()=>finishInput({status:'ok'}));cleanup();
});

it("rejects letterbox clicks and preserves text when manual input is refused", async () => {
  vi.mocked(api).mockClear();
  useAppStore.setState({sessionId:"session-1",runStatus:"idle",messages:[]});
  vi.mocked(api).mockImplementation(async url=>{
    if(url.endsWith('/preview'))return {status:'ok',frame_id:'issued-frame',result:{width:1000,height:500,image:{mime_type:'image/jpeg',data:'/9j/test'}}};
    if(url.endsWith('/action'))throw new Error('Displayed frame expired; no input sent');
    return {...computerState,runtime:'rust',lockedBy:'user',canControl:true,previewAvailable:true,frame:undefined};
  });
  render(<ComputerPanel/>);const img=await screen.findByAltText('Live desktop');
  vi.spyOn(img,'getBoundingClientRect').mockReturnValue({left:0,top:0,width:500,height:500,right:500,bottom:500,x:0,y:0,toJSON:()=>({})});
  Object.defineProperty(img,'naturalWidth',{value:1000});Object.defineProperty(img,'naturalHeight',{value:500});
  fireEvent.click(img,{clientX:200,clientY:20});
  expect(vi.mocked(api).mock.calls.some(([url])=>url.endsWith('/action'))).toBe(false);
  const field=screen.getByPlaceholderText('Type into the app…');fireEvent.change(field,{target:{value:'Keep this draft'}});fireEvent.keyDown(field,{key:'Enter'});
  expect(await screen.findByRole('alert')).toHaveTextContent('Displayed frame expired');expect(field).toHaveValue('Keep this draft');cleanup();
});
