#!/usr/bin/env python
# coding: utf-8


import json
from pprint import pprint


TREE = {
    '#': {
        #> # => {1, 2, 3, 4, 5, 6}
        None: [1, 2, 3, 4, 5, 6]
    },
    '' : {
        'ab': {
            #> /ab => {1, 2, 3}
            None: [1, 2, 3],
            'bc': {
                #> /ab/bc => {4, 5, 6}
                None: [4, 5, 6]
            },
            '+': {
                #> /ab/x
                #  /ab/yy
                #  /ab/zzz
                #    => {'x', 'y', 'z'}
                None: ['x', 'y', 'z']
            }
        }
    },
    'abc': {
        #> abc => {1, 3, 78}
        None: [1, 3, 78]
    },
}


def add(tree, topic, clients):
    parts = topic.split('/')
    for part in parts:
        if part not in tree:
            tree[part] = {}
        tree = tree[part]
    if None not in tree:
        tree[None] = []
    tree[None] += clients


def remove(tree, topic, clients):
    parts = topic.split('/')
    for part in parts:
        if part not in tree:
            tree[part] = {}
        tree = tree[part]
    for client in clients:
        tree[None].remove(client)


def main():
    tree = {}
    add(tree, '#', [1, 2, 3, 4])
    add(tree, '#', [5, 6])
    add(tree, '/ab', [1, 2, 3])
    add(tree, '/ab/bc', [4, 5])
    add(tree, '/ab/bc', [6])
    add(tree, '/ab/+', ['x', 'y', 'z'])
    add(tree, 'abc', [1, 3, 78])
    assert json.dumps(tree) == json.dumps(TREE)
    pprint(tree)

    print '-' * 60
    remove(tree, '/ab/bc', [5])
    TREE['']['ab']['bc'][None].remove(5)
    assert json.dumps(tree) == json.dumps(TREE)
    pprint(tree)


if __name__ == '__main__':
    main()
