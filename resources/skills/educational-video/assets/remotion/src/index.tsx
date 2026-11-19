import React from 'react';
import {AbsoluteFill, Composition, Sequence, Html5Audio, registerRoot, staticFile,
  useCurrentFrame, useVideoConfig, interpolate} from 'remotion';
import project from '../project-config';

const scenes = project.scenes;
const frames = (seconds: number) => Math.max(1, Math.round(seconds * project.fps));
const total = scenes.reduce((sum, scene) => sum + frames(scene.seconds), 0);

const Drawing: React.FC<{scene: typeof scenes[number]}> = ({scene}) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();
  const progress = interpolate(frame, [0, Math.max(1, scene.seconds * fps * .8)], [0, 1],
    {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'});
  return <AbsoluteFill style={{background: project.background, color: '#f8fafc',
    fontFamily: 'Arial, sans-serif', alignItems: 'center', justifyContent: 'center', padding: '7%'}}>
    <h1 style={{fontSize: 64, textAlign: 'center', margin: 0}}>{scene.title}</h1>
    <svg viewBox="0 0 800 420" style={{width: '85%', maxHeight: '55%'}}>
      <path d="M 70 340 L 70 50 M 70 340 L 750 340" fill="none" stroke="#94a3b8" strokeWidth="4"/>
      <path d="M 130 80 Q 400 560 680 80" pathLength="1" fill="none" stroke="#38bdf8"
        strokeWidth="8" strokeLinecap="round" strokeDasharray="1" strokeDashoffset={1-progress}/>
      <circle cx={130 + 270*progress} cy={80 + 240*(2*progress-progress*progress)} r="12" fill="#fbbf24"/>
    </svg>
    {'audio' in scene && scene.audio ? <Html5Audio src={staticFile(String(scene.audio))}/> : null}
  </AbsoluteFill>;
};

const Lesson = () => {
  const frame = useCurrentFrame();
  const caption = project.captions.find(c => frame >= c.start*project.fps && frame < c.end*project.fps);
  let offset = 0;
  return <AbsoluteFill>{scenes.map((scene, i) => {
    const start = offset; offset += frames(scene.seconds);
    return <Sequence key={i} from={start} durationInFrames={frames(scene.seconds)}><Drawing scene={scene}/></Sequence>;
  })}{caption && <div style={{position: 'absolute', bottom: '5%', left: '8%', right: '8%',
    textAlign: 'center', font: '40px Arial', color: 'white', background: '#000c', padding: 16}}>{caption.text}</div>}</AbsoluteFill>;
};
registerRoot(() => <Composition id="Lesson" component={Lesson} durationInFrames={total}
  fps={project.fps} width={project.width} height={project.height}/>);
