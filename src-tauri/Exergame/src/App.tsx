import {Component, createRef} from "react";
import {Channel, invoke} from "@tauri-apps/api/core";
import "./App.css";
import {error, info} from "@tauri-apps/plugin-log";
import {CameraPreview} from "./components/CameraPreview";
import {CameraSettings} from "./components/CameraSettings";
import type {CameraDescriptor} from "./components/CameraSettings";

type AppProps = Record<string, never>;

type AppState = {
    cameras: CameraDescriptor[];
    selectedCamera: string;
    resolutions: string[];
    selectedResolution: string;
    isLoadingCameras: boolean;
    isLoadingResolutions: boolean;
    isStartingCamera: boolean;
    isStoppingCamera: boolean;
    isStreaming: boolean;
    cameraError: string;
};

class App extends Component<AppProps, AppState> {
    state: AppState = {
        cameras: [],
        selectedCamera: "",
        resolutions: [],
        selectedResolution: "",
        isLoadingCameras: true,
        isLoadingResolutions: false,
        isStartingCamera: false,
        isStoppingCamera: false,
        isStreaming: false,
        cameraError: "",
    };

    private readonly previewRef = createRef<CameraPreview>();
    private frameChannel: Channel<ArrayBuffer> | null = null;
    private streamStarting = false;
    private streamStopping = false;

    componentDidMount() {
        void this.loadCameras();

        // Headless self-test hook (EXERGAME_SELFTEST_STREAM=1): expose the
        // real start flow so the packaged binary can be benchmarked with the
        // full UI running.
        (window as unknown as Record<string, unknown>).__exergameSelftest = () => {
            void this.startStreaming();
        };
    }

    componentWillUnmount() {
        void invoke("stop_rust_camera").catch((err) => error(`stop_rust_camera: ${err}`));
        this.detachStream();
    }

    private loadCameras = async () => {
        this.setState({isLoadingCameras: true, cameraError: ""});

        try {
            const cameras = await invoke<CameraDescriptor[]>("list_cameras");
            const selectedCamera = cameras.some((camera) => camera.index === this.state.selectedCamera)
                ? this.state.selectedCamera
                : cameras[0]?.index ?? "";

            this.setState({
                cameras,
                selectedCamera,
                isLoadingCameras: false,
            });

            if (selectedCamera) {
                void this.loadResolutions(selectedCamera);
            }
        } catch (err) {
            await error(`list_cameras: ${err}`);
            this.setState({
                isLoadingCameras: false,
                cameraError: String(err),
            });
        }
    };

    private loadResolutions = async (cameraIndex: string) => {
        this.setState({isLoadingResolutions: true, resolutions: [], selectedResolution: ""});

        try {
            const resolutions = await invoke<string[]>("list_resolutions", {cameraIndex});
            await info(`list_resolutions: ${resolutions.length} for camera ${cameraIndex}`);

            this.setState({
                resolutions,
                // Default to the smallest resolution (the list is sorted from
                // the largest one downwards).
                selectedResolution: resolutions[resolutions.length - 1] ?? "",
                isLoadingResolutions: false,
            });
        } catch (err) {
            await error(`list_resolutions: ${err}`);
            this.setState({
                isLoadingResolutions: false,
                cameraError: String(err),
            });
        }
    };

    private startStreaming = async () => {
        if (
            this.streamStarting
            || this.streamStopping
            || this.state.isStreaming
            || !this.state.selectedCamera
        ) {
            return;
        }

        this.streamStarting = true;
        this.setState({isStartingCamera: true, cameraError: ""});

        try {
            const onFrame = new Channel<ArrayBuffer>();
            onFrame.onmessage = (frame) => {
                this.previewRef.current?.pushFrame(frame);
            };

            this.frameChannel = onFrame;

            await invoke("start_rust_camera", {
                cameraIndex: this.state.selectedCamera,
                resolution: this.state.selectedResolution || null,
                onFrame,
            });
            await info(`start_rust_camera: started (${this.state.selectedCamera})`);
            this.setState({isStartingCamera: false, isStreaming: true});
        } catch (err) {
            await error(`start_rust_camera: ${err}`);
            this.detachStream();
            this.setState({
                isStartingCamera: false,
                cameraError: String(err),
            });
        } finally {
            this.streamStarting = false;
        }
    };

    private stopStreaming = async () => {
        if (this.streamStopping || !this.state.isStreaming) {
            return;
        }

        this.streamStopping = true;
        this.setState({isStoppingCamera: true, cameraError: ""});

        try {
            await invoke("stop_rust_camera");
            await info("stop_rust_camera: stopped");
            this.detachStream();
            this.setState({isStoppingCamera: false, isStreaming: false});
        } catch (err) {
            await error(`stop_rust_camera: ${err}`);
            this.setState({
                isStoppingCamera: false,
                cameraError: String(err),
            });
        } finally {
            this.streamStopping = false;
        }
    };

    private detachStream() {
        if (this.frameChannel) {
            this.frameChannel.onmessage = () => {
            };
            this.frameChannel = null;
        }

        this.previewRef.current?.clear();
    }

    render() {
        const {cameras, selectedCamera, resolutions, selectedResolution, isLoadingCameras, isLoadingResolutions, isStartingCamera, isStoppingCamera, isStreaming, cameraError} = this.state;

        return (
            <div className={`app-root${isStreaming ? " app-root-streaming" : ""}`}>
                <CameraPreview
                    ref={this.previewRef}
                    isStreaming={isStreaming}
                    isStoppingCamera={isStoppingCamera}
                    onStop={() => void this.stopStreaming()}
                />
                {!isStreaming && (
                        <CameraSettings
                            cameras={cameras}
                            selectedCamera={selectedCamera}
                            resolutions={resolutions}
                            selectedResolution={selectedResolution}
                            isLoadingCameras={isLoadingCameras}
                            isLoadingResolutions={isLoadingResolutions}
                            isStartingCamera={isStartingCamera}
                            isStoppingCamera={isStoppingCamera}
                            isStreaming={isStreaming}
                            cameraError={cameraError}
                            onCameraSelect={this.handleCameraSelect}
                            onResolutionSelect={this.handleResolutionSelect}
                            onRefresh={this.handleCameraRefresh}
                            onStart={() => void this.startStreaming()}
                            onStop={() => void this.stopStreaming()}
                        />
                    )}
            </div>
        );
    }

    private handleCameraSelect = (cameraIndex: string) => {
        if (!this.state.isStreaming) {
            this.setState({selectedCamera: cameraIndex, cameraError: ""});
            void this.loadResolutions(cameraIndex);
        }
    };

    private handleResolutionSelect = (resolution: string) => {
        if (!this.state.isStreaming) {
            this.setState({selectedResolution: resolution});
        }
    };

    private handleCameraRefresh = () => {
        void this.loadCameras();
    };
}

export default App;
