#!/usr/bin/env python

import os

from paho.mqtt.client import connack_string
import paho.mqtt.client as mqtt

DIR = os.path.dirname(os.path.abspath(__file__))


def main():
    def on_connect(client, userdata, flags, rc):
        print("Connection returned result: " + connack_string(rc))
        client.subscribe('abc')
        client.publish('abc', 'message to abc')

    def on_message(client, userdata, message):
        print("Received message '" + str(message.payload) + "' on topic '"
              + message.topic + "' with QoS " + str(message.qos))
        client.disconnect()

    client = mqtt.Client()
    client.on_connect = on_connect
    client.on_message = on_message
    ca_path = os.path.join(
        os.path.dirname(DIR),
        'experiment/tls-server/certificate/CA/root-ca.cert'
    )
    client.tls_set(ca_path)
    client.connect('localhost', 8883, 60)
    client.loop_forever()
    print('>>> test DONE')


if __name__ == '__main__':
    main()
