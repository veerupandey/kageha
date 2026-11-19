import data from './project.json';
export default data as {
  width: number; height: number; fps: number; background: string;
  scenes: {title: string; seconds: number; narration: string; audio?: string}[];
  captions: {start: number; end: number; text: string}[];
};
