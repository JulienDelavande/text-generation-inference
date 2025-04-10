import psutil
import time
from typing import Optional, Tuple, List
from threading import Thread, Event
from queue import Queue
import numpy as np

try:
    import pynvml
    NVML_AVAILABLE = True
except ImportError:
    NVML_AVAILABLE = False

class EnergyMonitor:
    def __init__(self):
        self.gpu_handles = []
        self.initialized = False
        self.measurement_thread = None
        self.stop_event = Event()
        self.power_queue = Queue()
        self.start_time = None
        self.end_time = None
        
    def initialize(self):
        if NVML_AVAILABLE:
            try:
                pynvml.nvmlInit()
                device_count = pynvml.nvmlDeviceGetCount()
                self.gpu_handles = [pynvml.nvmlDeviceGetHandleByIndex(i) for i in range(device_count)]
                self.initialized = True
            except Exception as e:
                logger.warning(f"Failed to initialize NVML: {e}")
                self.initialized = False

    def _measure_power(self):
        """Thread function to continuously measure power"""
        while not self.stop_event.is_set():
            try:
                # CPU power
                cpu_percent = psutil.cpu_percent()
                cpu_power = cpu_percent * 0.1  # in watts (adjust based on your CPU)
                
                # GPU power
                gpu_power = 0
                if self.initialized and NVML_AVAILABLE:
                    for handle in self.gpu_handles:
                        power = pynvml.nvmlDeviceGetPowerUsage(handle)
                        gpu_power += power / 1000.0  # Convert to watts
                
                # Store measurement with timestamp
                self.power_queue.put((time.time_ns(), cpu_power, gpu_power))
                
                # Sleep for a short interval (e.g., 1ms)
                time.sleep(0.001)
            except Exception as e:
                logger.warning(f"Error during power measurement: {e}")
                break

    def start_measurement(self):
        """Start continuous power measurement"""
        self.start_time = time.time_ns()
        self.stop_event.clear()
        self.power_queue = Queue()
        self.measurement_thread = Thread(target=self._measure_power)
        self.measurement_thread.start()

    def get_measurement(self) -> Tuple[Optional[float], Optional[float]]:
        """Get energy consumption since start_measurement was called"""
        self.end_time = time.time_ns()
        self.stop_event.set()
        if self.measurement_thread:
            self.measurement_thread.join()
            self.measurement_thread = None

        # Process all measurements
        measurements = []
        while not self.power_queue.empty():
            measurements.append(self.power_queue.get())

        if not measurements:
            return None, None

        # Convert to numpy arrays for easier processing
        timestamps, cpu_powers, gpu_powers = zip(*measurements)
        timestamps = np.array(timestamps)
        cpu_powers = np.array(cpu_powers)
        gpu_powers = np.array(gpu_powers)

        # Calculate energy by integrating power over time
        # Convert timestamps to seconds and calculate time differences
        timestamps_sec = timestamps / 1e9
        time_diffs = np.diff(timestamps_sec)
        
        # Calculate energy for each interval
        cpu_energy = np.sum(cpu_powers[:-1] * time_diffs)
        gpu_energy = np.sum(gpu_powers[:-1] * time_diffs)

        return cpu_energy, gpu_energy 