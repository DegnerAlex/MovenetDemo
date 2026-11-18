// Copyright 2026 Alex Degner (alex@ergofox.me)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

import {Component, createRef} from "react";
import {Channel, invoke} from "@tauri-apps/api/core";
import "./App.scss";
import {error, info} from "@tauri-apps/plugin-log";
import {CameraPreview} from "./components/CameraPreview";
import {CameraSettings} from "./components/CameraSettings";
import type {CameraDescriptor, ResolutionInfo} from "./components/CameraSettings";

type AppProps = Record<string, never>;

type AppState = {
    cameras: CameraDescriptor[];
    selectedCamera: string;
    resolutions: ResolutionInfo[];
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
            const resolutions = await invoke<ResolutionInfo[]>("list_resolutions", {cameraIndex});
            await info(`list_resolutions: ${resolutions.length} for camera ${cameraIndex}`);

            this.setState({
                resolutions,
                // Default to the smallest resolution (the list is sorted from
                // the largest one downwards).
                selectedResolution: resolutions[resolutions.length - 1]?.resolution ?? "",
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
                    <div className="app-scroll-area">
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
                    </div>
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
