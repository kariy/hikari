## How it works (roughly but tbh i dont know)

1. Task that will listen to the p2p network:
- listen to the latest extended header
- notify the other task that handles header downloads about the latest header
- notify only the latest header

2. Task that will sync the headers:
- get the latest header listened from the p2p network
- download all the headers up to the latest one
- download the headers in batches
