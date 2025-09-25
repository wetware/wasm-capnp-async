@0xc2420680fb470a77;

interface Echoer {
    echo @0 (msg :Text) -> (reply :Data);
}


interface EchoerProvider {
    echoer @0 () -> (echoer :Echoer);
}


