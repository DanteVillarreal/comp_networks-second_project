use std::collections::{BinaryHeap, VecDeque, HashMap};
use std::cmp::Ordering;
use rand::prelude::*;
use rand_distr::{Distribution, Exp};

#[derive(Debug, Clone)]
struct Packet {
    flow_id: usize,
    seq_num: u32,
    is_retransmission: bool,
}

#[derive(Debug)]
enum EventType {
    PacketArrival(Packet),
    ServiceCompletion(Packet),
    AckArrival { flow_id: usize, seq_num: u32 },
    RetransmissionTimeout { flow_id: usize, seq_num: u32 },
}

#[derive(Debug)]
struct Event {
    time: f64,
    event_type: EventType,
}

impl Ord for Event {
    fn cmp(&self, other: &Self) -> Ordering {
        self.time.partial_cmp(&other.time).unwrap().reverse()
    }
}

impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
    }
}

impl Eq for Event {}

#[derive(Clone)]
struct TCPFlow {
    cwnd: f64,
    rtt: f64,
    next_seq_num: u32,
    base_seq_num: u32,
    window: HashMap<u32, (f64, bool)>,  // (send_time, is_retransmission)
    dup_acks: HashMap<u32, u32>,
}

impl TCPFlow {
    fn new(rtt: f64) -> Self {
        TCPFlow {
            cwnd: 1.0,
            rtt,
            next_seq_num: 1,
            base_seq_num: 1,
            window: HashMap::new(),
            dup_acks: HashMap::new(),
        }
    }

    fn additive_increase(&mut self) {
        self.cwnd += 1.0;  // Increase by 1 segment per RTT
    }

    fn multiplicative_decrease(&mut self) {
        self.cwnd = (self.cwnd / 2.0).max(1.0);
    }

    fn can_send_new_packet(&self) -> bool {
        if self.next_seq_num >= self.base_seq_num {
            ((self.next_seq_num - self.base_seq_num) as f64) < self.cwnd
        } else {
            // Log potential sequence number issue
            println!("Warning: Sequence number inconsistency - next_seq_num: {}, base_seq_num: {}", 
                    self.next_seq_num, self.base_seq_num);
            false  // Don't allow sending when sequence numbers are inconsistent
        }
    }
}

struct Simulation {
    current_time: f64,
    events: BinaryHeap<Event>,
    flows: Vec<TCPFlow>,
    buffer: VecDeque<Packet>,
    buffer_size: usize,
    rng: ThreadRng,
    service_time_dist: Exp<f64>,
    throughput: Vec<(f64, Vec<usize>)>,
    goodput: Vec<(f64, Vec<usize>)>,
    packets_dropped: Vec<usize>,
    retransmissions: Vec<usize>,
    packets_in_flight: Vec<usize>,
}

impl Simulation {
    fn new(buffer_size: usize, service_rate: f64, rtts: &[f64]) -> Self {
        let num_flows = rtts.len();
        Simulation {
            current_time: 0.0,
            events: BinaryHeap::new(),
            flows: rtts.iter().map(|&rtt| TCPFlow::new(rtt)).collect(),
            buffer: VecDeque::new(),
            buffer_size,
            rng: thread_rng(),
            service_time_dist: Exp::new(service_rate).unwrap(),
            throughput: Vec::new(),
            goodput: Vec::new(),
            packets_dropped: vec![0; num_flows],
            retransmissions: vec![0; num_flows],
            packets_in_flight: vec![0; num_flows],
        }
    }

    fn schedule_packet_arrival(&mut self, packet: Packet, delay: f64) {
        self.events.push(Event {
            time: self.current_time + delay,
            event_type: EventType::PacketArrival(packet),
        });
    }
    
    fn schedule_service_completion(&mut self, packet: Packet, delay: f64) {
        self.events.push(Event {
            time: self.current_time + delay,
            event_type: EventType::ServiceCompletion(packet),
        });
    }

    fn schedule_ack_arrival(&mut self, flow_id: usize, seq_num: u32, delay: f64) {
        self.events.push(Event {
            time: self.current_time + delay,
            event_type: EventType::AckArrival { flow_id, seq_num },
        });
    }

    fn schedule_timeout(&mut self, flow_id: usize, seq_num: u32, delay: f64) {
        self.events.push(Event {
            time: self.current_time + delay,
            event_type: EventType::RetransmissionTimeout { flow_id, seq_num },
        });
    }

    fn handle_packet_drop(&mut self, packet: &Packet) {
        self.packets_dropped[packet.flow_id] += 1;
        // Schedule timeout but don't modify cwnd
        let flow = &self.flows[packet.flow_id];
        self.schedule_timeout(packet.flow_id, packet.seq_num, flow.rtt * 2.0);
    }

    fn handle_packet_arrival(&mut self, packet: Packet) {
        if packet.is_retransmission {
            self.retransmissions[packet.flow_id] += 1;
        }
        self.packets_in_flight[packet.flow_id] += 1;

        if self.buffer.len() >= self.buffer_size {
            self.handle_packet_drop(&packet);
            return;
        }

        let should_schedule_service = self.buffer.is_empty();
        self.buffer.push_back(packet.clone());
        
        if should_schedule_service {
            let service_time = self.service_time_dist.sample(&mut self.rng);
            self.schedule_service_completion(packet, service_time);
        }
    }

    fn handle_service_completion(&mut self, packet: Packet) {
        if let Some(_) = self.buffer.pop_front() {
            self.packets_in_flight[packet.flow_id] -= 1;
            
            let rtt = self.flows[packet.flow_id].rtt;
            self.schedule_ack_arrival(packet.flow_id, packet.seq_num, rtt);

            if let Some(next_packet) = self.buffer.front().cloned() {
                let service_time = self.service_time_dist.sample(&mut self.rng);
                self.schedule_service_completion(next_packet, service_time);
            }
        }
    }

    fn retransmit_from(&mut self, flow_id: usize, start_seq: u32, reduce_window: bool) {
        let mut flow = self.flows[flow_id].clone();
        
        if reduce_window {
            flow.multiplicative_decrease();
        }

        for seq in start_seq..flow.next_seq_num {
            let packet = Packet {
                flow_id,
                seq_num: seq,
                is_retransmission: true,
            };
            self.schedule_packet_arrival(packet, 0.0);
            
            if let Some((time, _)) = flow.window.get(&seq) {
                flow.window.insert(seq, (*time, true));
            }
        }
        
        self.flows[flow_id] = flow;
    }

    fn handle_ack_arrival(&mut self, flow_id: usize, seq_num: u32) {
        let mut flow = self.flows[flow_id].clone();
        
        if seq_num >= flow.base_seq_num {
            let old_cwnd = flow.cwnd;
            
            // Clear acknowledged packets from window and update base_seq_num
            flow.window.retain(|&seq, _| seq > seq_num);
            
            // Send new packets first before updating base_seq_num
            while flow.can_send_new_packet() {
                let packet = Packet {
                    flow_id,
                    seq_num: flow.next_seq_num,
                    is_retransmission: false,
                };
                self.schedule_packet_arrival(packet, 0.0);
                flow.window.insert(flow.next_seq_num, (self.current_time, false));
                flow.next_seq_num += 1;
            }
    
            // Now update base_seq_num after sending packets
            flow.base_seq_num = seq_num + 1;
            flow.dup_acks.clear();
            flow.additive_increase();
            
            if (flow.cwnd - old_cwnd).abs() > 0.1 {
                println!("Flow {}: cwnd changed from {:.2} to {:.2}", 
                        flow_id, old_cwnd, flow.cwnd);
            }
        } else {
            *flow.dup_acks.entry(seq_num).or_insert(0) += 1;
            if flow.dup_acks[&seq_num] >= 3 {
                let old_cwnd = flow.cwnd;
                self.retransmit_from(flow_id, seq_num + 1, true);
                println!("Flow {}: Triple duplicate ACK - cwnd reduced from {:.2} to {:.2}", 
                        flow_id, old_cwnd, flow.cwnd/2.0);
                flow.dup_acks.clear();
            }
        }
    
        self.flows[flow_id] = flow;
    }

    fn run(&mut self, duration: f64, measurement_interval: f64) {
        let mut next_measurement = measurement_interval;
        let mut interval_start = 0.0;
        let mut packets_sent = vec![0; self.flows.len()];
        let mut successful_transmissions = vec![0; self.flows.len()];
    
        // Clear previous measurements
        self.throughput.clear();
        self.goodput.clear();
    
        // Initialize flows with staggered start
        for flow_id in 0..self.flows.len() {
            let first_packet = Packet {
                flow_id,
                seq_num: 1,
                is_retransmission: false,
            };
            self.flows[flow_id].window.insert(1, (0.0, false));
            self.flows[flow_id].next_seq_num = 2;
            self.schedule_packet_arrival(first_packet, flow_id as f64 * 0.1);
        }
    
        while let Some(event) = self.events.pop() {
            self.current_time = event.time;
    
            if self.current_time >= duration {
                break;
            }
    
            // Handle event first
            match event.event_type {
                EventType::PacketArrival(packet) => {
                    packets_sent[packet.flow_id] += 1;
                    self.handle_packet_arrival(packet);
                }
                EventType::ServiceCompletion(packet) => {
                    successful_transmissions[packet.flow_id] += 1;
                    self.handle_service_completion(packet);
                }
                EventType::AckArrival { flow_id, seq_num } => {
                    self.handle_ack_arrival(flow_id, seq_num);
                }
                EventType::RetransmissionTimeout { flow_id, seq_num } => {
                    self.retransmit_from(flow_id, seq_num, false);
                }
            }
    
            // Then check for measurement interval
            while self.current_time >= next_measurement {
                let interval_duration = next_measurement - interval_start;
                
                if interval_duration > 0.0 {
                    // Record measurements
                    self.throughput.push((
                        next_measurement,
                        packets_sent.iter().map(|&x| x).collect()
                    ));
                    self.goodput.push((
                        next_measurement,
                        successful_transmissions.iter().map(|&x| x).collect()
                    ));
    
                    // Reset counters
                    packets_sent = vec![0; self.flows.len()];
                    successful_transmissions = vec![0; self.flows.len()];
                }
    
                // Update interval tracking
                interval_start = next_measurement;
                next_measurement += measurement_interval;
            }
        }
    
        // Record final measurements if there are any
        if interval_start < self.current_time {
            let interval_duration = self.current_time - interval_start;
            if interval_duration > 0.0 {
                self.throughput.push((
                    self.current_time,
                    packets_sent.iter().map(|&x| x).collect()
                ));
                self.goodput.push((
                    self.current_time,
                    successful_transmissions.iter().map(|&x| x).collect()
                ));
            }
        }
    }
}

fn main() {
    let buffer_size = 100;
    let service_rate = 50.0;  // Higher service rate to process packets faster
    let duration = 100.0;     // Keep original duration for proper analysis
    let measurement_interval = 0.2;  // More frequent measurements for better data resolution


    // Same RTTs
    println!("Running simulation with same RTTs (0.1, 0.1)");
    let mut sim = Simulation::new(buffer_size, service_rate, &[0.1, 0.1]);
    sim.run(duration, measurement_interval);

    println!("\nThroughput results (packets/second):");
    for (time, throughput) in sim.throughput {
        println!("Time {:.1}: Flow1={}, Flow2={}", time, throughput[0], throughput[1]);
    }

    println!("\nGoodput results (packets/second):");
    for (time, goodput) in sim.goodput {
        println!("Time {:.1}: Flow1={}, Flow2={}", time, goodput[0], goodput[1]);
    }

    println!("\nPackets dropped per flow:");
    for (flow_id, drops) in sim.packets_dropped.iter().enumerate() {
        println!("Flow {}: {} packets", flow_id + 1, drops);
    }

    println!("\nRetransmissions per flow:");
    for (flow_id, retrans) in sim.retransmissions.iter().enumerate() {
        println!("Flow {}: {} packets", flow_id + 1, retrans);
    }

    // Different RTTs
    println!("\nRunning simulation with different RTTs (0.1, 0.2)");
    let mut sim = Simulation::new(buffer_size, service_rate, &[0.1, 0.2]);
    sim.run(duration, measurement_interval);

    println!("\nThroughput results (packets/second):");
    for (time, throughput) in sim.throughput {
        println!("Time {:.1}: Flow1={}, Flow2={}", time, throughput[0], throughput[1]);
    }

    println!("\nGoodput results (packets/second):");
    for (time, goodput) in sim.goodput {
        println!("Time {:.1}: Flow1={}, Flow2={}", time, goodput[0], goodput[1]);
    }

    println!("\nPackets dropped per flow:");
    for (flow_id, drops) in sim.packets_dropped.iter().enumerate() {
        println!("Flow {}: {} packets", flow_id + 1, drops);
    }

    println!("\nRetransmissions per flow:");
    for (flow_id, retrans) in sim.retransmissions.iter().enumerate() {
        println!("Flow {}: {} packets", flow_id + 1, retrans);
    }
}