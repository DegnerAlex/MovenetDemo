import {type ChangeEvent, Component, createRef, type FormEvent} from "react";
import reactLogo from "./assets/react.svg";
import {Channel, invoke} from "@tauri-apps/api/core";
import "./App.css";
import {error, info} from "@tauri-apps/plugin-log";

type AppProps = Record<string, never>;

type AppState = {
    greetMsg: string;
    name: string;
};

const FRAME_HEADER_BYTES = 8;

class App extends Component<AppProps, AppState> {
    state: AppState = {
        greetMsg: "",
        name: "",
    };

    private readonly canvasRef = createRef<HTMLCanvasElement>();
    private imageData: ImageData | null = null;
    private pendingFrame: ArrayBuffer | null = null;
    private frameRequest: number | null = null;

    componentDidMount() {
        const onFrame = new Channel<ArrayBuffer>();
        onFrame.onmessage = (frame) => {
            // Keep only the latest frame; draw it on the next animation frame.
            this.pendingFrame = frame;
            if (this.frameRequest === null) {
                this.frameRequest = requestAnimationFrame(this.drawPendingFrame);
            }
        };

        invoke("start_rust_camera", {onFrame})
            .then(() => info("start_rust_camera: started"))
            .catch((err) => error(`start_rust_camera: ${err}`));
    }

    componentWillUnmount() {
        if (this.frameRequest !== null) {
            cancelAnimationFrame(this.frameRequest);
            this.frameRequest = null;
        }
        this.pendingFrame = null;
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
        const rgb = new Uint8Array(buffer, FRAME_HEADER_BYTES);

        if (width === 0 || height === 0 || rgb.byteLength < width * height * 3) {
            return;
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

        context.putImageData(this.imageData, 0, 0);
    }

    handleNameChange = (event: ChangeEvent<HTMLInputElement>) => {
        this.setState({name: event.currentTarget.value});
    };

    handleSubmit = (event: FormEvent<HTMLFormElement>) => {
        event.preventDefault();
        void this.greet();
    };

    async greet() {
        // Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
        this.setState({greetMsg: await invoke<string>("greet", {name: this.state.name})});
    }

    render() {
        const {greetMsg} = this.state;

        return (
            <main className="container">
                <h1>Welcome to Tauri + React</h1>

                <canvas ref={this.canvasRef} className="camera" aria-label="Camera preview"/>

                <div className="row">
                    <a href="https://vite.dev" target="_blank">
                        <img src="/vite.svg" className="logo vite" alt="Vite logo"/>
                    </a>
                    <a href="https://tauri.app" target="_blank">
                        <img src="/tauri.svg" className="logo tauri" alt="Tauri logo"/>
                    </a>
                    <a href="https://react.dev" target="_blank">
                        <img src={reactLogo} className="logo react" alt="React logo"/>
                    </a>
                </div>
                <p>Click on the Tauri, Vite, and React logos to learn more.</p>

                <form className="row" onSubmit={this.handleSubmit}>
                    <input
                        id="greet-input"
                        onChange={this.handleNameChange}
                        placeholder="Enter a name..."
                    />
                    <button type="submit">Greet</button>
                </form>
                <p>{greetMsg}</p>
            </main>
        );
    }
}

export default App;
