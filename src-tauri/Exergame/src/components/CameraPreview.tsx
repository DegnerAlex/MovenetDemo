import {Component, createRef} from "react";
import {Button, Tag} from "react-bulma-components";
import type {PoseEstimate, PoseStats} from "./PoseSkeleton";
import {PoseSkeleton} from "./PoseSkeleton";
import {error} from "@tauri-apps/plugin-log";

type Resolution = {
    width: number;
    height: number;
};

// Sliding-average window for recognition time and FPS on the badge.
const SMOOTHING_WINDOW = 10;
const FPS_SMOOTHING_WINDOW = 20;

// Frame header: width, height and JSON tail length with the recognized pose (u32 big-endian).
const FRAME_HEADER_BYTES = 12;

type CameraPreviewProps = {
    isStreaming: boolean;
    isStoppingCamera: boolean;
    onStop: () => void;
};

type CameraPreviewState = {
    resolution: Resolution | null;
    pose: PoseEstimate | null;
    poseStats: PoseStats | null;
    mirror: boolean;
};

/**
 * Renders the camera stream: keeps only the latest frame, paints it on the
 * next animation frame and shows the telemetry badges (resolution,
 * recognition time/FPS, execution provider). The skeleton overlay is drawn
 * by the `PoseSkeleton` component stacked over the video canvas.
 *
 * The parent feeds frames through `pushFrame()` and calls `clear()` when the
 * stream stops.
 */
export class CameraPreview extends Component<CameraPreviewProps, CameraPreviewState> {
    state: CameraPreviewState = {
        resolution: null,
        pose: null,
        poseStats: null,
        mirror: true,
    };

    private readonly canvasRef = createRef<HTMLCanvasElement>();
    private imageData: ImageData | null = null;
    private pendingFrame: ArrayBuffer | null = null;
    private frameRequest: number | null = null;
    private inferenceMsWindow: number[] = [];
    private maxFpsWindow: number[] = [];

    // Called by the parent for every frame received from the camera channel.
    pushFrame(buffer: ArrayBuffer) {
        // Keep only the latest frame; draw it on the next animation frame.
        this.pendingFrame = buffer;
        if (this.frameRequest === null) {
            this.frameRequest = requestAnimationFrame(this.drawPendingFrame);
        }
    }

    // Resets the preview when the stream stops or fails.
    clear() {
        if (this.frameRequest !== null) {
            cancelAnimationFrame(this.frameRequest);
            this.frameRequest = null;
        }

        this.pendingFrame = null;
        this.imageData = null;
        this.resetPoseWindows();
        this.setState({resolution: null, pose: null, poseStats: null});

        const canvas = this.canvasRef.current;
        const context = canvas?.getContext("2d");
        if (canvas && context) {
            context.clearRect(0, 0, canvas.width, canvas.height);
        }
    }

    componentWillUnmount() {
        if (this.frameRequest !== null) {
            cancelAnimationFrame(this.frameRequest);
            this.frameRequest = null;
        }
    }

    private drawPendingFrame = () => {
        this.frameRequest = null;

        const frame = this.pendingFrame;
        this.pendingFrame = null;

        if (frame) {
            this.drawFrame(frame);
        }
    };

    private drawFrame(buffer: ArrayBuffer) {
        const canvas = this.canvasRef.current;
        if (!canvas || buffer.byteLength < FRAME_HEADER_BYTES) {
            return;
        }

        const header = new DataView(buffer);
        const width = header.getUint32(0, false);
        const height = header.getUint32(4, false);
        const poseLength = header.getUint32(8, false);
        const rgbLength = width * height * 3;
        const poseOffset = FRAME_HEADER_BYTES + rgbLength;

        if (width === 0 || height === 0 || buffer.byteLength < poseOffset + poseLength) {
            return;
        }

        const rgb = new Uint8Array(buffer, FRAME_HEADER_BYTES, rgbLength);

        // The resolution arrives in every frame header; redraw the badge only
        // when it actually changed.
        const resolution = this.state.resolution;
        if (!resolution || resolution.width !== width || resolution.height !== height) {
            this.setState({resolution: {width, height}});
        }

        if (canvas.width !== width || canvas.height !== height) {
            canvas.width = width;
            canvas.height = height;
            this.imageData = null;
        }

        const context = canvas.getContext("2d");
        if (!context) {
            return;
        }

        if (!this.imageData) {
            this.imageData = context.createImageData(width, height);
        }

        // The camera sends RGB, but ImageData needs RGBA, so expand the pixels.
        const rgba = this.imageData.data;
        for (let src = 0, dst = 0; dst < rgba.length; src += 3, dst += 4) {
            rgba[dst] = rgb[src];
            rgba[dst + 1] = rgb[src + 1];
            rgba[dst + 2] = rgb[src + 2];
            rgba[dst + 3] = 255;
        }

        this.applyPose(buffer, poseOffset, poseLength);
        context.putImageData(this.imageData, 0, 0);
    }

    private applyPose(buffer: ArrayBuffer, offset: number, length: number) {
        if (length === 0) {
            this.resetPoseWindows();
            this.setState({pose: null, poseStats: null});
            return;
        }

        try {
            const json = new TextDecoder().decode(new Uint8Array(buffer, offset, length));

            const pose = JSON.parse(json) as PoseEstimate;
            this.setState({
                pose,
                poseStats: {
                    inferenceMs: this.pushSmoothed(this.inferenceMsWindow, pose.inferenceMs, SMOOTHING_WINDOW),
                    maxFps: this.pushSmoothed(this.maxFpsWindow, pose.maxFps, FPS_SMOOTHING_WINDOW),
                    provider: pose.provider,
                },
            });
        } catch (ex) {
            error(`??${ex}`);
        }
    }

    // Keeps a window of the last `size` values and returns the average.
    private pushSmoothed(window: number[], value: number, size: number): number {
        window.push(value);
        if (window.length > size) {
            window.shift();
        }

        const sum = window.reduce((acc, item) => acc + item, 0);
        return sum / window.length;
    }

    private resetPoseWindows() {
        this.inferenceMsWindow = [];
        this.maxFpsWindow = [];
    }

    render() {
        const {resolution, pose, poseStats, mirror} = this.state;
        const {isStreaming, isStoppingCamera, onStop} = this.props;

        // FPS badge color: <30 error, 30..60 warning, 60..90 success, >90 info.
        const fpsColor = poseStats
            ? poseStats.maxFps > 90
                ? "info"
                : poseStats.maxFps >= 60
                    ? "success"
                    : poseStats.maxFps >= 30
                        ? "warning"
                        : "danger"
            : undefined;

        return (
            <div className="camera-preview">
                <div className="camera-canvas-stack">
                    <canvas
                        ref={this.canvasRef}
                        className="camera"
                        aria-label="Camera preview"
                        style={{transform: mirror ? "scaleX(-1)" : undefined}}
                    />
                    <PoseSkeleton pose={pose} mirror={mirror}/>
                    {!isStreaming && (
                        <div className="camera-placeholder" aria-hidden="true">
                            <svg viewBox="0 0 240 160" width="220" height="147" role="presentation">
                                {/* Webcam body */}
                                <rect x="60" y="28" width="120" height="92" rx="18"
                                      fill="#16233a" stroke="#3b4d6b" strokeWidth="2"/>
                                {/* Lens */}
                                <circle cx="120" cy="74" r="32" fill="#0d1626" stroke="#4c6079" strokeWidth="2"/>
                                <circle cx="120" cy="74" r="22" fill="#101a2c"/>
                                {/* Skeleton figure inside the lens */}
                                <circle cx="120" cy="66" r="5.5" fill="#dfe7f2"/>
                                <line x1="120" y1="72" x2="120" y2="88" stroke="#dfe7f2" strokeWidth="2.5" strokeLinecap="round"/>
                                <line x1="120" y1="77" x2="110" y2="87" stroke="#dfe7f2" strokeWidth="2.5" strokeLinecap="round"/>
                                <line x1="120" y1="77" x2="130" y2="87" stroke="#dfe7f2" strokeWidth="2.5" strokeLinecap="round"/>
                                <line x1="120" y1="88" x2="112" y2="98" stroke="#dfe7f2" strokeWidth="2.5" strokeLinecap="round"/>
                                <line x1="120" y1="88" x2="128" y2="98" stroke="#dfe7f2" strokeWidth="2.5" strokeLinecap="round"/>
                                {/* Recording dot */}
                                <circle cx="167" cy="42" r="4" fill="#38bdf8"/>
                                {/* Stand */}
                                <rect x="104" y="120" width="32" height="8" rx="4" fill="#3b4d6b"/>
                                <rect x="112" y="128" width="16" height="12" rx="3" fill="#2c3c57"/>
                            </svg>
                            <span>Camera is off — start streaming to see the preview</span>
                        </div>
                    )}
                </div>
                <div className={`mt-2 is-flex is-flex-wrap-wrap is-justify-content-space-between is-align-items-center${isStreaming ? " status-card" : ""}`}
                     style={{gap: "0.5rem", flexShrink: 0, position: "relative", zIndex: 1}}>
                    <div className="is-flex is-flex-wrap-wrap is-align-items-center" style={{gap: "0.5rem"}}>
                        {resolution && (
                            <Tag>{`${resolution.width}×${resolution.height}`}</Tag>
                        )}
                        {poseStats && (
                            <Tag>
                                {`Recognition: ${poseStats.inferenceMs.toFixed(0)} ms`}
                            </Tag>
                        )}
                        {poseStats && (
                            <Tag color={fpsColor}>
                                {`Max: ${poseStats.maxFps.toFixed(1)} FPS`}
                            </Tag>
                        )}
                        {poseStats?.provider && (
                            <Tag color={poseStats.provider === "GPU" ? "success" : "warning"}>
                                {`MoveNet: ${poseStats.provider}`}
                            </Tag>
                        )}
                    </div>
                    <div className="is-flex is-align-items-center" style={{gap: "0.5rem"}}>
                        {isStreaming && (
                            <>
                                <Button
                                    size="small"
                                    color={mirror ? "info" : undefined}
                                    aria-pressed={mirror}
                                    onClick={this.handleMirrorToggle}
                                >
                                    Mirror
                                </Button>
                                <Button
                                    size="small"
                                    color="danger"
                                    onClick={onStop}
                                    disabled={isStoppingCamera}
                                >
                                    {isStoppingCamera ? "Stopping…" : "Stop streaming"}
                                </Button>
                            </>
                        )}
                    </div>
                </div>
            </div>
        );
    }

    private handleMirrorToggle = () => {
        this.setState((state) => ({mirror: !state.mirror}));
    };
}
