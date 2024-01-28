// - peer id
// - gossipsub topic id
// - set up gossipsub behaviour
// - set up swarm
// - set up kademlia dht
// - dial to trusted peers

use libp2p::Multiaddr;

fn trusted_peers() -> impl Iterator<Item = Multiaddr> {
    let peers : &[_] = &[ "/dns4/da-bridge-mocha-4.celestia-mocha.com/tcp/2121/p2p/12D3KooWCBAbQbJSpCpCGKzqz3rAN4ixYbc63K68zJg9aisuAajg",
        "/dns4/da-bridge-mocha-4-2.celestia-mocha.com/tcp/2121/p2p/12D3KooWK6wJkScGQniymdWtBwBuU36n6BRXp9rCDDUD6P5gJr3G",
        "/dns4/da-full-1-mocha-4.celestia-mocha.com/tcp/2121/p2p/12D3KooWCUHPLqQXZzpTx1x3TAsdn3vYmTNDhzg66yG8hqoxGGN8",
        "/dns4/da-full-2-mocha-4.celestia-mocha.com/tcp/2121/p2p/12D3KooWR6SHsXPkkvhCRn6vp1RqSefgaT1X1nMNvrVjU2o3GoYy",
    ];

    peers.iter().map(|addr| addr.parse().unwrap())
}

fn with_trusted_peers<F>(func: F)
where
    F: FnOnce(Box<dyn Iterator<Item = Multiaddr>>),
{
    func(Box::new(trusted_peers()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{trusted_peers, with_trusted_peers};
    use libp2p::futures::StreamExt;
    use libp2p::identity::{ed25519, Keypair};
    use libp2p::multiaddr::Protocol;
    use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
    use libp2p::PeerId;
    use libp2p::{autonat, identify, kad, ping};
    use libp2p::{dns, noise, tcp, yamux};
    use libp2p::{gossipsub, SwarmBuilder};
    use tokio::select;
    use tokio::time::{interval_at, Instant};
    use tracing::{info, trace};
    use tracing_subscriber::{fmt, EnvFilter};

    #[derive(NetworkBehaviour)]
    struct Behaviour {
        autonat: autonat::Behaviour,
        ping: ping::Behaviour,
        identify: identify::Behaviour,
        // header_ex: HeaderExBehaviour<S>,
        gossipsub: gossipsub::Behaviour,
        kademlia: kad::Behaviour<kad::store::MemoryStore>,
    }

    #[tokio::test]
    async fn main() {
        let subscriber = fmt::Subscriber::builder()
            .with_env_filter(EnvFilter::try_new("info").unwrap())
            .finish();

        tracing::subscriber::set_global_default(subscriber).unwrap();

        let local_keypair = Keypair::from(ed25519::Keypair::generate());
        let local_peer_id = PeerId::from_public_key(&local_keypair.public());

        // set up gossipsub protocol
        let gossipsub = {
            use libp2p::gossipsub::{
                Behaviour, ConfigBuilder, IdentTopic, MessageAuthenticity, ValidationMode,
            };

            // Set the message authenticity - How we expect to publish messages
            // Here we expect the publisher to sign the message with their key.
            let message_authenticity = MessageAuthenticity::Signed(local_keypair.clone());

            let config = ConfigBuilder::default()
                .validation_mode(ValidationMode::Strict)
                .validate_messages()
                .build()
                .unwrap();

            // build a gossipsub network behaviour
            let mut gossipsub: Behaviour = Behaviour::new(message_authenticity, config).unwrap();
            gossipsub
                .subscribe(&IdentTopic::new("/mocha/header-sub/v0.0.1"))
                .unwrap();

            gossipsub
        };

        let autonat = autonat::Behaviour::new(local_peer_id, autonat::Config::default());
        let ping = ping::Behaviour::new(ping::Config::default());

        let identify =
            identify::Behaviour::new(identify::Config::new(String::new(), local_keypair.public()));

        let kademlia = {
            use libp2p::kad::store::MemoryStore;
            use libp2p::kad::{Behaviour, Config};
            use libp2p::StreamProtocol;

            let local_peer_id = PeerId::from_public_key(&local_keypair.public());
            let mut config = Config::default();

            let protocol_id = format!("/celestia/mocha-4/kad/1.0.0");
            let protocol_id = StreamProtocol::try_from_owned(protocol_id).unwrap();

            config.set_protocol_names(vec![protocol_id]);

            let store = MemoryStore::new(local_peer_id);
            let mut kademlia = Behaviour::with_config(local_peer_id, store, config);

            with_trusted_peers(|peers| {
                for addr in peers {
                    let peer_id = addr.iter().find_map(|proto| match proto {
                        Protocol::P2p(peer_id) => Some(peer_id),
                        _ => None,
                    });

                    if let Some(peer_id) = peer_id {
                        kademlia.add_address(&peer_id, addr.to_owned());
                    }
                }
            });

            kademlia
        };

        let behaviour = Behaviour {
            autonat,
            gossipsub,
            identify,
            kademlia,
            ping,
        };

        let mut swarm = SwarmBuilder::with_existing_identity(local_keypair.clone())
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .unwrap()
            .with_quic()
            .with_behaviour(|_| behaviour)
            .expect("Moving behaviour doesn't fail")
            .with_swarm_config(|config| {
                // TODO: Refactor code to avoid being idle. This can be done by preloading a
                // handler. This is how they fixed Kademlia:
                // https://github.com/libp2p/rust-libp2p/pull/4675/files
                config.with_idle_connection_timeout(Duration::from_secs(15))
            })
            .build();

        with_trusted_peers(|peers| {
            for addr in peers {
                swarm.dial(addr).unwrap();
            }
        });

        // let mut report_interval ;
        let mut kademlia_interval = interval_at(Instant::now(), Duration::from_secs(30));
        let mut kademlia_last_bootstrap = Instant::now();

        // Initiate discovery
        let _ = swarm.behaviour_mut().kademlia.bootstrap();
        const KADEMLIA_BOOTSTRAP_PERIOD: Duration = Duration::from_secs(5 * 60);

        loop {
            select! {
                _ = kademlia_interval.tick() => {
                    if  kademlia_last_bootstrap.elapsed() > KADEMLIA_BOOTSTRAP_PERIOD
                    {
                        info!("Running kademlia bootstrap procedure.");
                        let _ = swarm.behaviour_mut().kademlia.bootstrap();
                        kademlia_last_bootstrap = Instant::now();
                    }
                }

                ev = swarm.select_next_some() => {
                    println!("event: {:?}", ev);

                    match ev {
                           SwarmEvent::Behaviour(ev) => match ev {
                               BehaviourEvent::Identify(ev) => {
                                   match ev {
                                       identify::Event::Received { peer_id, info } => {
                                           // Inform Kademlia about the listening addresses
                                           // TODO: Remove this when rust-libp2p#4302 is implemented
                                           for addr in info.listen_addrs {
                                               self.swarm
                                                   .behaviour_mut()
                                                   .kademlia
                                                   .add_address(&peer_id, addr);
                                           }
                                       }
                                       _ => trace!("Unhandled identify event"),
                                   }
                               },
                               BehaviourEvent::Gossipsub(ev) => self.on_gossip_sub_event(ev).await,
                               BehaviourEvent::Kademlia(ev) => self.on_kademlia_event(ev).await?,
                               BehaviourEvent::Autonat(_)
                               | BehaviourEvent::Ping(_)
                                => {}
                           },
                           SwarmEvent::ConnectionEstablished {
                               peer_id,
                               connection_id,
                               endpoint,
                               ..
                           } => {
                               self.on_peer_connected(peer_id, connection_id, endpoint);
                           }
                           SwarmEvent::ConnectionClosed {
                               peer_id,
                               connection_id,
                               ..
                           } => {
                               self.on_peer_disconnected(peer_id, connection_id);
                           }
                           _ => {}
                       }
                },
            }
        }
    }
}
